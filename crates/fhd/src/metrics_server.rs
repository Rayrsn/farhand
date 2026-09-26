//! An opt-in Prometheus endpoint.
//!
//! Deliberately hand-rolled over a `TcpListener`: this serves exactly one
//! static text endpoint, and adding a web framework to the daemon's dependency
//! tree to do that would be a poor trade. Nothing here accepts a request body,
//! runs a handler, or takes a header — the request line is read and answered.
//!
//! Everything exported already exists in [`ServerContext`] and the metrics
//! probes; this only formats it. The endpoint binds separately from the agent
//! port so it can be firewalled independently, and it is off unless
//! `--metrics-port` is passed.

use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::session::ServerContext;

/// Start serving `/metrics` on `listener` until the process exits.
pub async fn serve_metrics(listener: tokio::net::TcpListener, ctx: Arc<ServerContext>) {
    let addr = listener
        .local_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| "?".into());
    tracing::info!("Prometheus metrics listening on http://{addr}/metrics");
    loop {
        let (mut socket, _peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                tracing::warn!("metrics: accept failed: {e}");
                continue;
            }
        };
        let ctx = Arc::clone(&ctx);
        // One short-lived task per scrape; a scrape is cheap and this must
        // never be able to stall the accept loop.
        tokio::spawn(async move {
            let mut buf = [0u8; 1024];
            let read = match socket.read(&mut buf).await {
                Ok(n) => n,
                Err(_) => return,
            };
            let request = String::from_utf8_lossy(&buf[..read]);
            let path = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or("/");

            let (status, content_type, body) = match path {
                "/metrics" => ("200 OK", "text/plain; version=0.0.4", render(&ctx)),
                "/healthz" => ("200 OK", "text/plain; charset=utf-8", "ok\n".to_string()),
                _ => (
                    "404 Not Found",
                    "text/plain; charset=utf-8",
                    "try /metrics or /healthz\n".to_string(),
                ),
            };

            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.flush().await;
        });
    }
}

/// Render the whole exposition in Prometheus text format.
pub fn render(ctx: &ServerContext) -> String {
    let mut out = Exposition::new();
    let active_runs = ctx
        .max_runs
        .saturating_sub(ctx.semaphore.available_permits());
    let queued = ctx.queue_depth.load(std::sync::atomic::Ordering::Relaxed);

    out.gauge("farhand_up", "1 if the agent is serving metrics", &[], 1.0);
    out.gauge(
        "farhand_uptime_seconds",
        "Seconds since the agent started",
        &[],
        ctx.start_time.elapsed().as_secs_f64(),
    );
    out.gauge(
        "farhand_active_runs",
        "Runs executing right now",
        &[],
        active_runs as f64,
    );
    out.gauge(
        "farhand_max_runs",
        "Configured concurrency limit",
        &[],
        ctx.max_runs as f64,
    );
    out.gauge(
        "farhand_queue_depth",
        "Runs waiting on a project lock or a run slot",
        &[],
        queued as f64,
    );
    out.gauge(
        "farhand_max_queued_runs",
        "Configured queue limit",
        &[],
        ctx.max_queued_runs as f64,
    );
    out.gauge(
        "farhand_workspaces",
        "Persistent workspaces on this agent",
        &[],
        crate::metrics::get_workspaces_count(&ctx.workdir_root).unwrap_or(0) as f64,
    );
    out.gauge(
        "farhand_cpu_count",
        "Usable CPUs on the agent host",
        &[],
        crate::metrics::get_cpu_count() as f64,
    );

    if let Ok(space) = workspace::get_disk_space(&ctx.workdir_root) {
        out.gauge(
            "farhand_disk_free_bytes",
            "Free space on the workspace volume",
            &[],
            space.available_bytes as f64,
        );
        out.gauge(
            "farhand_disk_total_bytes",
            "Total space on the workspace volume",
            &[],
            space.total_bytes as f64,
        );
    }

    if let Some(loads) = crate::metrics::get_load_averages() {
        for (interval, value) in [("1", loads[0]), ("5", loads[1]), ("15", loads[2])] {
            out.gauge(
                "farhand_load_average",
                "Host load average",
                &[("interval", interval)],
                value,
            );
        }
    }

    let (used, total) = crate::metrics::get_memory_info();
    if let Some(total) = total {
        out.gauge(
            "farhand_memory_total_bytes",
            "Physical memory on the agent host",
            &[],
            total as f64,
        );
    }
    if let Some(used) = used {
        out.gauge(
            "farhand_memory_used_bytes",
            "Physical memory in use on the agent host",
            &[],
            used as f64,
        );
    }

    // Active builds, one sample per run. The id is high-cardinality by nature
    // (it is the run id), so it is deliberately NOT a label: a run that ends
    // would leave a stale series. Project and duration are enough to alert on.
    {
        let map = ctx
            .active_builds
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let by_project: std::collections::BTreeMap<&str, usize> = map.values().fold(
            std::collections::BTreeMap::new(),
            |mut acc, (project, ..)| {
                *acc.entry(project.as_str()).or_insert(0) += 1;
                acc
            },
        );
        for (project, count) in by_project {
            out.gauge(
                "farhand_active_builds",
                "Runs in flight, by project",
                &[("project", project)],
                count as f64,
            );
        }
    }
    out.text
}

/// Accumulates the exposition, declaring each metric family exactly once.
///
/// Prometheus requires a single `# HELP`/`# TYPE` pair per metric *name*: a
/// family exposed with several label values (load averages per interval, for
/// example) declares the type once and then emits several samples. Emitting
/// the pair again per sample is a scrape-time parse error, not a cosmetic one.
struct Exposition {
    text: String,
    declared: std::collections::HashSet<String>,
}

impl Exposition {
    fn new() -> Self {
        Exposition {
            text: String::with_capacity(2048),
            declared: std::collections::HashSet::new(),
        }
    }

    fn gauge(&mut self, name: &str, help: &str, labels: &[(&str, &str)], value: f64) {
        if self.declared.insert(name.to_string()) {
            self.text
                .push_str(&format!("# HELP {name} {help}\n# TYPE {name} gauge\n"));
        }
        if labels.is_empty() {
            self.text.push_str(&format!("{name} {value}\n"));
        } else {
            let rendered: Vec<String> = labels
                .iter()
                .map(|(k, v)| format!("{k}=\"{}\"", escape_label_value(v)))
                .collect();
            self.text
                .push_str(&format!("{name}{{{}}} {value}\n", rendered.join(",")));
        }
    }
}

/// Escape a label value for the exposition format.
///
/// A project name comes from a directory name, and Unix allows newlines in
/// those: an unescaped one would split the sample and corrupt every line after
/// it.
fn escape_label_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn exposition_is_well_formed_for_every_series() {
        let ctx = crate::session::ServerContext {
            expected_token: None,
            workdir_root: std::env::temp_dir(),
            custom_shell: None,
            semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
            lock_manager: workspace::WorkspaceLockManager::new(),
            tags: vec![],
            queue_depth: Arc::new(std::sync::atomic::AtomicUsize::new(2)),
            max_runs: 4,
            min_disk_bytes: 0,
            cas_store: None,
            start_time: std::time::Instant::now(),
            active_builds: Arc::new(std::sync::Mutex::new(HashMap::new())),
            connection_limiter: Arc::new(tokio::sync::Semaphore::new(32)),
            max_queued_runs: 16,
        };

        let text = render(&ctx);

        // A gauge is a bare number, so a float is always fine, but the labels
        // and the sample must line up: `name{a="b"} value`, never a dangling
        // brace or a missing value (which Prometheus silently drops).
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            assert!(
                !line.contains('{') || line.contains("} "),
                "malformed: {line}"
            );
            let value = line.rsplit(' ').next().expect("a sample has a value");
            assert!(
                value.parse::<f64>().is_ok(),
                "sample value is not a number: {line}"
            );
        }

        // Every series must be declared, and each name declared only once —
        // a duplicate HELP/TYPE block is a parse error for strict scrapers.
        let mut declared: Vec<&str> = Vec::new();
        for line in text.lines().filter(|l| l.starts_with("# TYPE ")) {
            let name = line
                .split_whitespace()
                .nth(2)
                .expect("TYPE line has a name");
            assert!(!declared.contains(&name), "duplicate TYPE for {name}");
            declared.push(name);
        }
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            let name = line.split(['{', ' ']).next().expect("sample has a name");
            assert!(
                declared.contains(&name),
                "sample `{name}` has no # TYPE declaration"
            );
        }

        assert!(text.contains("farhand_up 1"));
        assert!(text.contains("farhand_queue_depth 2"));
        assert!(text.contains("farhand_max_runs 4"));
    }

    #[test]
    fn active_builds_are_reported_per_project() {
        let ctx = crate::session::ServerContext {
            expected_token: None,
            workdir_root: std::env::temp_dir(),
            custom_shell: None,
            semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
            lock_manager: workspace::WorkspaceLockManager::new(),
            tags: vec![],
            queue_depth: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            max_runs: 4,
            min_disk_bytes: 0,
            cas_store: None,
            start_time: std::time::Instant::now(),
            active_builds: Arc::new(std::sync::Mutex::new(HashMap::from([
                (
                    "run-1".to_string(),
                    (
                        "api".to_string(),
                        vec!["make".to_string()],
                        std::time::Instant::now(),
                        "c".to_string(),
                    ),
                ),
                (
                    "run-2".to_string(),
                    (
                        "api".to_string(),
                        vec!["make".to_string()],
                        std::time::Instant::now(),
                        "c".to_string(),
                    ),
                ),
                (
                    "run-3".to_string(),
                    (
                        "web".to_string(),
                        vec!["npm".to_string()],
                        std::time::Instant::now(),
                        "c".to_string(),
                    ),
                ),
            ]))),
            connection_limiter: Arc::new(tokio::sync::Semaphore::new(32)),
            max_queued_runs: 16,
        };

        let text = render(&ctx);
        assert!(
            text.contains(r#"farhand_active_builds{project="api"} 2"#),
            "{text}"
        );
        assert!(
            text.contains(r#"farhand_active_builds{project="web"} 1"#),
            "{text}"
        );
        // Run ids are deliberately not labels: a finished run would leave a
        // stale series behind forever.
        assert!(!text.contains("run-1"), "run ids must not become labels");
    }

    #[test]
    fn label_values_are_escaped() {
        let mut out = Exposition::new();
        // A project name comes from a directory name, and Unix allows quotes,
        // backslashes, and newlines in those.
        out.gauge("x", "h", &[("project", "we\"ird\nname")], 1.0);
        assert!(
            out.text.contains(r#"project="we\"ird\nname""#),
            "{}",
            out.text
        );
        // Every sample must stay on one line, or everything after it is lost.
        assert_eq!(out.text.lines().filter(|l| !l.starts_with('#')).count(), 1);
    }
}
