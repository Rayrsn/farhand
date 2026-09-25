use clap::Parser;
use std::path::PathBuf;
use tokio::net::TcpListener;
use tracing::{info, warn};

#[derive(Parser, Debug)]
#[command(
    name = "fhd",
    version,
    about = "Farhand daemon: persistent remote build agent"
)]
struct Cli {
    #[arg(long, default_value = "0.0.0.0:9876", help = "Address to listen on")]
    listen: String,

    #[arg(long, env = "FARHAND_TOKEN", help = "Shared authentication token")]
    token: Option<String>,

    #[arg(
        long = "allow-unauthenticated",
        action = clap::ArgAction::SetTrue,
        help = "Accept connections without a token (NEVER expose to untrusted networks)"
    )]
    allow_unauthenticated: bool,

    #[arg(long, help = "Root directory for persistent workspaces")]
    workdir: Option<PathBuf>,

    #[arg(long, help = "Custom shell invocation (e.g. '/bin/sh -c')")]
    shell: Option<String>,

    #[arg(
        long = "max-concurrent-runs",
        alias = "max-runs",
        help = "Maximum parallel runs across projects (default: CPU count)"
    )]
    max_concurrent_runs: Option<usize>,

    #[arg(
        long = "max-connections",
        help = "Maximum concurrent client connections (default: 32, 0 = unlimited)"
    )]
    max_connections: Option<usize>,

    #[arg(
        long = "tag",
        action = clap::ArgAction::Append,
        help = "Agent capability tags (repeatable, e.g. '--tag lan --tag gpu')"
    )]
    tags: Vec<String>,

    #[arg(
        long = "log-level",
        default_value = "info",
        help = "Log level (trace, debug, info, warn, error)"
    )]
    log_level: String,

    #[arg(
        long = "log-format",
        default_value = "text",
        help = "Log format ('text' or 'json')"
    )]
    log_format: String,

    #[arg(
        long = "max-disk-gb",
        help = "Maximum total disk capacity allocated for workspaces in GB (e.g. 100)"
    )]
    max_disk_gb: Option<f64>,

    #[arg(
        long = "workspace-ttl-days",
        help = "Maximum days of inactivity before an ephemeral branch workspace is pruned (e.g. 7)"
    )]
    workspace_ttl_days: Option<u64>,

    #[arg(
        long = "min-disk-gb",
        env = "FARHAND_MIN_DISK_GB",
        default_value = "2.5",
        help = "Minimum free disk space in GB required before accepting builds (default: 2.5)"
    )]
    min_disk_gb: f64,

    #[arg(
        long = "gc-interval-secs",
        default_value = "3600",
        help = "Background garbage collection interval in seconds (default: 3600, 0 to disable)"
    )]
    gc_interval_secs: u64,

    #[arg(
        long = "cas-ttl-days",
        default_value = "30",
        help = "Days of inactivity before a CAS object is evicted (default: 30, 0 = never)"
    )]
    cas_ttl_days: u64,

    #[arg(
        long = "cas-max-gb",
        help = "Maximum total size of the content-addressable store in GB (default: unlimited)"
    )]
    cas_max_gb: Option<f64>,

    #[arg(
        long = "cas-dir",
        env = "FARHAND_CAS_DIR",
        help = "Directory for content-addressable storage (default: <workdir>/cas/objects)"
    )]
    cas_dir: Option<PathBuf>,

    #[arg(
        long = "no-cas",
        action = clap::ArgAction::SetTrue,
        help = "Disable global content-addressable storage (CAS)"
    )]
    no_cas: bool,

    #[arg(
        long = "tls",
        action = clap::ArgAction::SetTrue,
        help = "Enable TLS encryption"
    )]
    tls: bool,

    #[arg(long = "tls-cert", help = "Path to PEM-encoded TLS certificate chain")]
    tls_cert: Option<PathBuf>,

    #[arg(long = "tls-key", help = "Path to PEM-encoded TLS private key")]
    tls_key: Option<PathBuf>,

    #[arg(
        long = "tls-client-ca",
        help = "Path to PEM-encoded CA certificate to require and verify client certificates (mTLS)"
    )]
    tls_client_ca: Option<PathBuf>,

    #[arg(
        long = "tls-auto",
        action = clap::ArgAction::SetTrue,
        help = "Automatically generate an in-memory self-signed certificate and log fingerprint"
    )]
    tls_auto: bool,
}

fn setup_tracing(level_str: &str, format_str: &str) {
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(level_str))
        .unwrap_or_else(|_| EnvFilter::new("info"));

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_thread_ids(false);

    if format_str.eq_ignore_ascii_case("json") {
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt_layer.json())
            .init();
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt_layer)
            .init();
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    setup_tracing(&cli.log_level, &cli.log_format);

    // Fail fast on authentication misconfiguration before binding a socket.
    if let Err(msg) = fhd::validate_start_config(cli.token.as_deref(), cli.allow_unauthenticated) {
        eprintln!("error: {msg}");
        std::process::exit(2);
    }

    // A whitespace-only --shell used to panic the connection task; reject it
    // at startup instead.
    if let Some(shell) = &cli.shell {
        if fhd::parse_custom_shell(shell).is_none() {
            eprintln!("error: --shell '{}' is empty or whitespace-only", shell);
            std::process::exit(2);
        }
    }

    let tls_enabled = cli.tls || cli.tls_auto || cli.tls_cert.is_some();
    if cli.allow_unauthenticated {
        warn!(
            "AUTHENTICATION DISABLED (--allow-unauthenticated): any client that can \
             reach {} can execute arbitrary commands on this host. Intended for \
             development only.",
            cli.listen
        );
        if fhd::is_exposed_bind(&cli.listen) {
            warn!("Listening on a non-loopback address without authentication.");
        }
    }
    if cli.token.is_some() && !tls_enabled && fhd::is_exposed_bind(&cli.listen) {
        warn!(
            "Tokens travel in cleartext over raw TCP. Use --tls / --tls-auto, or \
             front the port with a tunnel (ssh -L, cloudflared) on untrusted networks."
        );
    }

    let listener = TcpListener::bind(&cli.listen).await?;
    let workdir = cli
        .workdir
        .unwrap_or_else(workspace::default_workspaces_dir);

    info!("Farhand daemon listening on {}", cli.listen);
    info!("Persistent workspaces root: {}", workdir.display());

    // The workspace lock manager is shared between the CLI-created GC task
    // and the connection handlers, so GC never races an active run.
    let lock_manager = workspace::WorkspaceLockManager::new();

    // Spawn background garbage collection task if enabled
    if cli.gc_interval_secs > 0 {
        let gc_workdir = workdir.clone();
        let max_bytes = cli
            .max_disk_gb
            .map(|gb| (gb * 1024.0 * 1024.0 * 1024.0) as u64);
        let ttl = cli
            .workspace_ttl_days
            .map(|d| std::time::Duration::from_secs(d * 86400));
        let interval_dur = std::time::Duration::from_secs(cli.gc_interval_secs);
        let gc_lock_manager = lock_manager.clone();

        // CAS GC inputs (objects dir mirrors CasStore::new's layout).
        let cas_objects_dir = cli
            .cas_dir
            .clone()
            .unwrap_or_else(|| workdir.join("cas"))
            .join("objects");
        let cas_ttl = if cli.cas_ttl_days == 0 {
            None
        } else {
            Some(std::time::Duration::from_secs(cli.cas_ttl_days * 86400))
        };
        let cas_max_bytes = cli
            .cas_max_gb
            .map(|gb| (gb * 1024.0 * 1024.0 * 1024.0) as u64);
        let cas_enabled = !cli.no_cas;

        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval_dur);
            loop {
                ticker.tick().await;
                // Snapshot locked projects (async), then run the blocking GC
                // passes off the async runtime so directory sizing never
                // stalls the executor.
                let locked: std::collections::HashSet<String> = gc_lock_manager
                    .locked_projects()
                    .await
                    .into_iter()
                    .collect();
                let dir = gc_workdir.clone();
                let cas_dir = cas_objects_dir.clone();
                let (report, cas_report) = tokio::task::spawn_blocking(move || {
                    let ws =
                        workspace::run_garbage_collection(&dir, max_bytes, ttl, &|name: &str| {
                            locked.contains(name)
                        });
                    let cas = if cas_enabled {
                        workspace::gc_cas(&cas_dir, cas_max_bytes, cas_ttl)
                    } else {
                        workspace::CasGcReport::default()
                    };
                    (ws, cas)
                })
                .await
                .unwrap_or_default();
                if report.workspaces_deleted > 0
                    || report.caches_trimmed_bytes > 0
                    || cas_report.objects_deleted > 0
                {
                    info!(
                        "GC: Pruned {} workspaces ({} bytes) and {} CAS objects ({} bytes); \
                         trimmed {} cache bytes. Disk remaining: {} bytes",
                        report.workspaces_deleted,
                        report.workspaces_deleted_bytes,
                        cas_report.objects_deleted,
                        cas_report.bytes_freed,
                        report.caches_trimmed_bytes,
                        report.remaining_disk_bytes
                    );
                }
            }
        });
    }

    let min_disk_bytes = (cli.min_disk_gb * 1024.0 * 1024.0 * 1024.0) as u64;

    let tls_acceptor = if tls_enabled {
        let (cert_pem, key_pem) =
            if cli.tls_auto || (cli.tls_cert.is_none() && cli.tls_key.is_none()) {
                info!("Generating self-signed TLS certificate (ephemeral)...");
                let san = vec![
                    "localhost".to_string(),
                    "127.0.0.1".to_string(),
                    fhd::get_hostname(),
                ];
                let self_cert = protocol::generate_self_signed_cert(san)?;
                info!(
                    "Self-signed TLS SHA-256 fingerprint: {}",
                    self_cert.fingerprint
                );
                (self_cert.cert_pem, self_cert.key_pem)
            } else {
                let cert_path = cli
                    .tls_cert
                    .as_ref()
                    .ok_or("--tls-cert required when --tls is set without --tls-auto")?;
                let key_path = cli
                    .tls_key
                    .as_ref()
                    .ok_or("--tls-key required when --tls is set without --tls-auto")?;
                let cert_pem = std::fs::read_to_string(cert_path)?;
                let key_pem = std::fs::read_to_string(key_path)?;
                let parsed_certs = protocol::parse_pem_certs(&cert_pem)?;
                if let Some(first) = parsed_certs.first() {
                    let fp = protocol::compute_cert_fingerprint(first.as_ref());
                    info!("Server TLS SHA-256 fingerprint: {}", fp);
                }
                (cert_pem, key_pem)
            };

        let client_ca_pem = if let Some(ref ca_path) = cli.tls_client_ca {
            Some(std::fs::read_to_string(ca_path)?)
        } else {
            None
        };

        let server_config =
            protocol::create_server_config(&cert_pem, &key_pem, client_ca_pem.as_deref())?;
        info!("TLS enabled on listener");
        Some(protocol::TlsAcceptor::from(server_config))
    } else {
        None
    };

    fhd::run_server(
        listener,
        cli.token,
        workdir,
        cli.shell,
        cli.max_concurrent_runs,
        cli.tags,
        Some(min_disk_bytes),
        cli.cas_dir,
        cli.no_cas,
        tls_acceptor,
        cli.max_connections,
        Some(lock_manager),
    )
    .await?;
    Ok(())
}
