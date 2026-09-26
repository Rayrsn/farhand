mod cli;

use clap::Parser;
use cli::{AgentAction, Cli, Subcommands, TemplateAction};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, size};
use notify::{Config as NotifyConfig, RecommendedWatcher, RecursiveMode, Watcher};
use protocol::{
    decode_json, read_frame, write_frame, write_json_frame, FileEntry, HelloAckPayload,
    HelloPayload, LogPayload, ManifestPayload, MsgType, NeedPayload, PortClosePayload,
    PortDataPayload, PortOpenPayload, PutTemplatePayload, ResizePayload, ResultPayload, RunPayload,
    CURRENT_PROTOCOL_VERSION,
};
use std::collections::HashMap;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::exit;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

const EXIT_INFRA_ERROR: i32 = 125;

/// Compute the environment forwarded to the agent (see [`fh::envfilter`]).
/// Explicit `-e KEY=VAL` overrides win over the denylist.
fn collect_forward_env(
    no_env_flag: bool,
    config_forward_env: bool,
    config_env: &std::collections::HashMap<String, String>,
    cli_env: &[String],
) -> Option<std::collections::HashMap<String, String>> {
    fh::envfilter::collect_forward_env(
        std::env::vars(),
        no_env_flag,
        config_forward_env,
        config_env,
        cli_env,
    )
}

fn collect_toolchain(
    config_toolchain: &std::collections::HashMap<String, String>,
    cli_toolchain: &[String],
) -> Option<std::collections::HashMap<String, String>> {
    let mut map = config_toolchain.clone();
    for entry in cli_toolchain {
        if let Some((k, v)) = entry.split_once('=') {
            map.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    if map.is_empty() {
        None
    } else {
        Some(map)
    }
}

#[derive(Debug, Default)]
struct Telemetry {
    scan_duration: std::time::Duration,
    upload_duration: std::time::Duration,
    remote_duration: std::time::Duration,
    download_duration: std::time::Duration,
    files_scanned: usize,
    files_uploaded: usize,
    bytes_uploaded: u64,
    bytes_downloaded: u64,
}

impl Telemetry {
    fn print_summary(&self, project: &str, host: &str) {
        println!("=== Farhand Execution Summary ===");
        println!("[Project]       {}", project);
        println!("[Agent]         {}", host);
        println!("---------------------------------");
        println!(
            "[Scan]          {} files in {:?}",
            self.files_scanned, self.scan_duration
        );
        println!(
            "[Delta Sync]    {} files ({} bytes) in {:?}",
            self.files_uploaded, self.bytes_uploaded, self.upload_duration
        );
        println!("[Remote Build]  Completed in {:?}", self.remote_duration);
        println!(
            "[Artifacts]     {} bytes in {:?}",
            self.bytes_downloaded, self.download_duration
        );
        println!("---------------------------------");
    }
}

fn setup_tracing(level_str: &str, format_str: &str) {
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level_str));
    let fmt_layer = tracing_subscriber::fmt::layer().with_target(false);

    if format_str == "json" {
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

fn detect_git_branch(dir: &std::path::Path) -> Option<String> {
    let head_path = dir.join(".git").join("HEAD");
    if let Ok(content) = std::fs::read_to_string(&head_path) {
        let trimmed = content.trim();
        if let Some(branch_ref) = trimmed.strip_prefix("ref: refs/heads/") {
            return Some(branch_ref.to_string());
        }
    }

    if let Ok(output) = std::process::Command::new("git")
        .current_dir(dir)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
    {
        if output.status.success() {
            let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !branch.is_empty() && branch != "HEAD" {
                return Some(branch);
            }
        }
    }

    None
}

fn sanitize_branch_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

struct TerminalGuard {
    active: bool,
}

impl TerminalGuard {
    fn enter() -> Self {
        let active = if std::io::stdin().is_terminal() {
            enable_raw_mode().is_ok()
        } else {
            false
        };
        Self { active }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = disable_raw_mode();
        }
    }
}

use fh::should_ignore_path;

async fn start_port_forward<W: AsyncWrite + Unpin + Send + 'static>(
    writer: Arc<Mutex<W>>,
    port_channels: Arc<Mutex<HashMap<u32, tokio::sync::mpsc::Sender<Vec<u8>>>>>,
    local_port: u16,
    remote_port: u16,
    next_channel_id: Arc<AtomicU32>,
) -> Result<tokio::task::JoinHandle<()>, std::io::Error> {
    let listener = TcpListener::bind(("127.0.0.1", local_port)).await?;
    println!(
        "[Port Forward] Listening on 127.0.0.1:{} -> remote:{}",
        local_port, remote_port
    );

    let handle = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let channel_id = next_channel_id.fetch_add(1, Ordering::SeqCst);
            let (mut tcp_read, mut tcp_write) = stream.into_split();
            let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(128);
            port_channels.lock().await.insert(channel_id, tx);

            let writer_clone = writer.clone();
            let channels_clone = port_channels.clone();

            let open = PortOpenPayload {
                channel_id,
                target_port: remote_port,
            };
            {
                let mut w = writer_clone.lock().await;
                if write_json_frame(&mut *w, MsgType::PortOpen, &open)
                    .await
                    .is_err()
                {
                    continue;
                }
            }

            tokio::spawn(async move {
                let w_in = writer_clone.clone();
                let read_task = tokio::spawn(async move {
                    let mut buf = [0u8; 8192];
                    while let Ok(n) = tcp_read.read(&mut buf).await {
                        if n == 0 {
                            break;
                        }
                        let payload = PortDataPayload {
                            channel_id,
                            data: buf[..n].to_vec(),
                        };
                        let mut w = w_in.lock().await;
                        if write_json_frame(&mut *w, MsgType::PortData, &payload)
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    let close = PortClosePayload { channel_id };
                    let mut w = w_in.lock().await;
                    let _ = write_json_frame(&mut *w, MsgType::PortClose, &close).await;
                });

                let write_task = tokio::spawn(async move {
                    while let Some(chunk) = rx.recv().await {
                        if tcp_write.write_all(&chunk).await.is_err() {
                            break;
                        }
                    }
                });

                let _ = tokio::join!(read_task, write_task);
                channels_clone.lock().await.remove(&channel_id);
            });
        }
    });

    Ok(handle)
}

#[allow(clippy::too_many_arguments)]
/// Everything one remote invocation needs.
///
/// This replaces an 18-positional-argument signature: call sites now read as
/// a labelled struct literal, so it is obvious which knob is which, and
/// adding a flag no longer means touching three call sites and a signature.
/// The body destructures it immediately, so the implementation below still
/// refers to plain local names.
struct RunParams<'a> {
    host: &'a str,
    token: &'a str,
    project_name: &'a str,
    project_dir: &'a Path,
    command: &'a [String],
    outputs: Option<Vec<String>>,
    out_dir: &'a Path,
    template: Option<String>,
    no_cache: bool,
    run_env: Option<HashMap<String, String>>,
    run_toolchain: Option<HashMap<String, String>>,
    tty: bool,
    forwards: &'a [String],
    compression: Option<String>,
    verbose: bool,
    tls_config: Option<&'a config::TlsConfig>,
    telemetry: &'a mut Telemetry,
}

/// Run the HELLO / HELLO_ACK handshake on an established stream.
///
/// Shared by the build path and the one-shot command paths so the wire
/// sequence, the diagnostics, and the exit code stay identical: any
/// handshake problem is an infrastructure failure (125), never a build
/// failure. `compressions` is the client's offer (`None` = none offered).
/// `wait_queued` lets the caller tolerate the agent's QUEUED frames — sent
/// while it waits on a project lock or the global run queue — before the
/// acknowledgement arrives.
async fn perform_handshake<S>(
    stream: &mut S,
    token: &str,
    project: &str,
    compressions: Option<Vec<String>>,
    wait_queued: bool,
) -> HelloAckPayload
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let hello = HelloPayload {
        token: token.to_string(),
        project: project.to_string(),
        protocol_version: CURRENT_PROTOCOL_VERSION,
        compressions,
    };
    if let Err(e) = write_json_frame(stream, MsgType::Hello, &hello).await {
        eprintln!("Error: failed to send HELLO handshake: {}", e);
        exit(EXIT_INFRA_ERROR);
    }

    let ack: HelloAckPayload = loop {
        let (msg_type, payload) = match read_frame(stream).await {
            Ok(f) => f,
            Err(e) => {
                eprintln!("Error: failed to read HELLO_ACK: {}", e);
                exit(EXIT_INFRA_ERROR);
            }
        };
        match msg_type {
            MsgType::Queued if wait_queued => {
                if let Ok(q) = decode_json::<protocol::QueuedPayload>(&payload) {
                    println!(
                        "[queued] Agent busy ({}), position in queue: {}. Waiting for lock...",
                        q.reason, q.position
                    );
                }
            }
            MsgType::HelloAck => {
                let parsed: HelloAckPayload = match decode_json(&payload) {
                    Ok(a) => a,
                    Err(e) => {
                        eprintln!("Error: invalid HELLO_ACK payload: {}", e);
                        exit(EXIT_INFRA_ERROR);
                    }
                };
                break parsed;
            }
            other => {
                eprintln!("Protocol error: expected HELLO_ACK, received {:?}", other);
                exit(EXIT_INFRA_ERROR);
            }
        }
    };

    if !ack.ok {
        eprintln!(
            "Authentication failed: {}",
            ack.error.as_deref().unwrap_or("rejected by agent")
        );
        exit(EXIT_INFRA_ERROR);
    }
    ack
}

async fn run_build(p: RunParams<'_>) -> Result<i32, Box<dyn std::error::Error + Send + Sync>> {
    let RunParams {
        host,
        token,
        project_name,
        project_dir,
        command,
        outputs,
        out_dir,
        template,
        no_cache,
        run_env,
        run_toolchain,
        tty,
        forwards,
        compression,
        verbose,
        tls_config,
        telemetry,
    } = p;
    if verbose {
        println!("=== Farhand Remote Runner ===");
        println!("Connecting to agent at: {}", host);
        println!("Project: {} ({})", project_name, project_dir.display());
        println!("Remote command: {:?}", command);
        if let Some(env_map) = &run_env {
            let mut names: Vec<&str> = env_map.keys().map(String::as_str).collect();
            names.sort_unstable();
            println!(
                "Env forwarding: {} variable(s) — use --print-env to list names (values are never shown)",
                names.len()
            );
            if !names.is_empty() {
                println!("  forwarded: {}", names.join(", "));
            }
        } else {
            println!("Env forwarding: none (use -e KEY=VAL to pass specific variables)");
        }
        if tty {
            println!("Terminal mode: PTY allocated");
        }
        if !forwards.is_empty() {
            println!("Port forwards: {:?}", forwards);
        }
    }

    // 1. Connect TCP/TLS to agent
    let mut stream = match fh::connect_to_agent(host, tls_config).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "Error: unable to reach agent at {} ({}).\nHint: ensure fhd is running and reachable.",
                host, e
            );
            exit(EXIT_INFRA_ERROR);
        }
    };

    // 2. Handshake: send HELLO, wait out any QUEUED frames, read HELLO_ACK
    let client_compressions = if let Some(c) = &compression {
        vec![c.to_string()]
    } else {
        vec!["zstd".to_string(), "gzip".to_string(), "none".to_string()]
    };
    let ack = perform_handshake(
        &mut stream,
        token,
        project_name,
        Some(client_compressions),
        true,
    )
    .await;

    let negotiated_compression = ack
        .compression
        .clone()
        .unwrap_or_else(|| "gzip".to_string());
    let compression_algo = fileset::CompressionAlgo::from_str_opt(Some(&negotiated_compression));
    if verbose {
        println!(
            "[Negotiation] Wire compression: {}",
            compression_algo.as_str()
        );
    }

    // 3. Scan local files & Build MANIFEST
    let scan_start = Instant::now();
    let scanned_files = match fileset::scan(project_dir, &[]) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("Error: failed to scan project files: {}", e);
            exit(EXIT_INFRA_ERROR);
        }
    };

    let manifest_files: Vec<FileEntry> = scanned_files
        .values()
        .map(|meta| FileEntry {
            path: meta.path.clone(),
            hash: meta.hash.clone(),
            size: meta.size,
            mode: meta.mode,
        })
        .collect();

    let manifest = ManifestPayload {
        files: manifest_files,
    };

    telemetry.scan_duration = scan_start.elapsed();
    telemetry.files_scanned = manifest.files.len();

    if let Err(e) = write_json_frame(&mut stream, MsgType::Manifest, &manifest).await {
        eprintln!("Error: failed to send MANIFEST frame: {}", e);
        exit(EXIT_INFRA_ERROR);
    }

    // 4. Receive NEED frame (handling optional QUEUED frames first)
    let need: NeedPayload = loop {
        let (msg_type, payload) = match read_frame(&mut stream).await {
            Ok(f) => f,
            Err(e) => {
                eprintln!("Error: failed to read NEED frame: {}", e);
                exit(EXIT_INFRA_ERROR);
            }
        };

        match msg_type {
            MsgType::Queued => {
                let queued: protocol::QueuedPayload = match decode_json(&payload) {
                    Ok(q) => q,
                    Err(e) => {
                        eprintln!("Error: invalid QUEUED payload: {}", e);
                        exit(EXIT_INFRA_ERROR);
                    }
                };
                println!(
                    "[farhand] Build queued on agent (reason: {}, position: {}). Waiting for remote workspace...",
                    queued.reason, queued.position
                );
            }
            MsgType::Need => {
                let n: NeedPayload = match decode_json(&payload) {
                    Ok(n) => n,
                    Err(e) => {
                        eprintln!("Error: invalid NEED payload: {}", e);
                        exit(EXIT_INFRA_ERROR);
                    }
                };
                break n;
            }
            other => {
                eprintln!(
                    "Protocol error: expected NEED or QUEUED frame, received {:?}",
                    other
                );
                exit(EXIT_INFRA_ERROR);
            }
        }
    };

    // 5. Pack & Upload delta files
    let sync_start = Instant::now();
    if need.want.is_empty() {
        if verbose {
            println!(
                "[Delta Sync] Remote workspace is completely up to date. 0 files to transfer!"
            );
        }
        if let Err(e) = write_frame(&mut stream, MsgType::Files, &[]).await {
            eprintln!("Error: failed to send empty FILES frame: {}", e);
            exit(EXIT_INFRA_ERROR);
        }
        telemetry.upload_duration = sync_start.elapsed();
        telemetry.files_uploaded = 0;
        telemetry.bytes_uploaded = 0;
    } else {
        if verbose {
            println!(
                "[Delta Sync] Agent requested {} changed/missing files. Packing delta archive...",
                need.want.len()
            );
        }
        let tar_gz = match fileset::pack_tar_with_algo(project_dir, &need.want, compression_algo) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("Error: failed to pack delta files into archive: {}", e);
                exit(EXIT_INFRA_ERROR);
            }
        };

        let upload_len = tar_gz.len() as u64;
        let want_len = need.want.len();

        if verbose {
            println!(
                "[Delta Sync] Uploading {} bytes (compressed) across {} files in {:?}",
                upload_len,
                want_len,
                sync_start.elapsed()
            );
        }

        if let Err(e) = write_frame(&mut stream, MsgType::Files, &tar_gz).await {
            eprintln!("Error: failed to send FILES frame: {}", e);
            exit(EXIT_INFRA_ERROR);
        }

        telemetry.upload_duration = sync_start.elapsed();
        telemetry.files_uploaded = want_len;
        telemetry.bytes_uploaded = upload_len;
    }

    // 6. Split stream for concurrent communication
    let (mut read_half, write_half) = tokio::io::split(stream);
    let shared_writer = Arc::new(Mutex::new(write_half));

    // 7. Setup Reverse Port Forwarding
    let port_channels = Arc::new(Mutex::new(
        HashMap::<u32, tokio::sync::mpsc::Sender<Vec<u8>>>::new(),
    ));
    let next_channel_id = Arc::new(AtomicU32::new(1));
    let mut forward_handles = Vec::new();

    for fwd in forwards {
        if let Some((local_str, remote_str)) = fwd.split_once(':') {
            if let (Ok(local_port), Ok(remote_port)) =
                (local_str.parse::<u16>(), remote_str.parse::<u16>())
            {
                match start_port_forward(
                    shared_writer.clone(),
                    port_channels.clone(),
                    local_port,
                    remote_port,
                    next_channel_id.clone(),
                )
                .await
                {
                    Ok(handle) => forward_handles.push(handle),
                    Err(e) => eprintln!(
                        "Warning: failed to forward port {}:{}: {}",
                        local_port, remote_port, e
                    ),
                }
            }
        }
    }

    // 8. Send RUN frame
    let (cols, rows) = if tty {
        match size() {
            Ok((c, r)) => (Some(c), Some(r)),
            Err(_) => (None, None),
        }
    } else {
        (None, None)
    };

    let run = RunPayload {
        argv: command.to_vec(),
        outputs,
        cwd: None,
        template,
        no_cache,
        env: run_env,
        toolchain: run_toolchain,
        tty,
        cols,
        rows,
        raw_stdio: None,
    };

    let remote_start = Instant::now();
    {
        let mut w = shared_writer.lock().await;
        if let Err(e) = write_json_frame(&mut *w, MsgType::Run, &run).await {
            eprintln!("Error: failed to send RUN frame: {}", e);
            exit(EXIT_INFRA_ERROR);
        }
    }

    // 9. Interactive PTY setup if tty is active
    let _term_guard = if tty {
        Some(TerminalGuard::enter())
    } else {
        None
    };

    let (stdin_tx, mut stdin_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    let mut stdin_task = None;
    let mut stdin_forwarder = None;

    if tty {
        let writer_for_stdin = shared_writer.clone();
        stdin_task = Some(tokio::task::spawn_blocking(move || {
            use std::io::Read;
            let mut stdin = std::io::stdin();
            let mut buf = [0u8; 1024];
            while let Ok(n) = stdin.read(&mut buf) {
                if n == 0 {
                    break;
                }
                if stdin_tx.blocking_send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        }));

        stdin_forwarder = Some(tokio::spawn(async move {
            while let Some(chunk) = stdin_rx.recv().await {
                let mut w = writer_for_stdin.lock().await;
                if write_frame(&mut *w, MsgType::Stdin, &chunk).await.is_err() {
                    break;
                }
            }
        }));
    }

    // 10. Receive streamed LOG, RESULT, and optional ARTIFACTS frames
    let mut exit_code = 1;
    let mut stdout = std::io::stdout();
    let mut stderr = std::io::stderr();
    let mut received_result = false;

    #[cfg(unix)]
    let mut sigwinch = if tty {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change()).ok()
    } else {
        None
    };

    loop {
        let (msg_type, payload) = tokio::select! {
            _ = async {
                #[cfg(unix)]
                {
                    if let Some(ref mut s) = sigwinch {
                        s.recv().await;
                        return;
                    }
                }
                std::future::pending::<()>().await
            } => {
                if let Ok((cols, rows)) = size() {
                    let resize = ResizePayload { cols, rows };
                    let mut w = shared_writer.lock().await;
                    let _ = write_json_frame(&mut *w, MsgType::Resize, &resize).await;
                }
                continue;
            }
            _ = tokio::signal::ctrl_c() => {
                if verbose {
                    eprintln!("\n[Signal] Process interrupted locally. Aborting remote execution...");
                }
                exit_code = 130;
                break;
            }
            res = read_frame(&mut read_half) => {
                match res {
                    Ok(f) => f,
                    Err(protocol::FrameError::UnexpectedEof) => {
                        if received_result {
                            break;
                        }
                        eprintln!("Error: connection dropped by agent before completion");
                        exit(EXIT_INFRA_ERROR);
                    }
                    Err(e) => {
                        if received_result {
                            break;
                        }
                        eprintln!("Error: connection dropped by agent: {}", e);
                        exit(EXIT_INFRA_ERROR);
                    }
                }
            }
        };

        match msg_type {
            MsgType::Log => {
                if let Ok(log) = decode_json::<LogPayload>(&payload) {
                    if log.stream == "stderr" {
                        let _ = stderr.write_all(log.data.as_bytes());
                        let _ = stderr.flush();
                    } else {
                        let _ = stdout.write_all(log.data.as_bytes());
                        let _ = stdout.flush();
                    }
                }
            }
            MsgType::PortData => {
                if let Ok(pd) = decode_json::<PortDataPayload>(&payload) {
                    let map = port_channels.lock().await;
                    if let Some(tx) = map.get(&pd.channel_id) {
                        let _ = tx.send(pd.data).await;
                    }
                }
            }
            MsgType::PortClose => {
                if let Ok(pc) = decode_json::<PortClosePayload>(&payload) {
                    port_channels.lock().await.remove(&pc.channel_id);
                }
            }
            MsgType::Result => {
                telemetry.remote_duration = remote_start.elapsed();
                if let Ok(res) = decode_json::<ResultPayload>(&payload) {
                    exit_code = res.exit_code;
                    received_result = true;
                    if let Some(err_msg) = res.error {
                        eprintln!("Remote error: {}", err_msg);
                    }
                    if exit_code != 0 {
                        break;
                    }
                } else {
                    eprintln!("Protocol error: failed to decode RESULT payload");
                    exit(EXIT_INFRA_ERROR);
                }
            }
            MsgType::Artifacts => {
                let extract_start = Instant::now();
                if let Err(e) = fileset::unpack_tar(out_dir, &payload) {
                    eprintln!("Error: failed to extract build artifacts: {}", e);
                    exit(EXIT_INFRA_ERROR);
                }
                telemetry.download_duration = extract_start.elapsed();
                telemetry.bytes_downloaded = payload.len() as u64;

                if verbose {
                    println!(
                        "[Artifacts] Extracted {} bytes into {} in {:?}",
                        payload.len(),
                        out_dir.display(),
                        telemetry.download_duration
                    );
                }
                break;
            }
            _ => {}
        }
    }

    for h in forward_handles {
        h.abort();
    }
    if let Some(h) = stdin_task {
        h.abort();
    }
    if let Some(h) = stdin_forwarder {
        h.abort();
    }

    Ok(exit_code)
}

/// Watch mode: run the build once, then rebuild on every local change.
///
/// Extracted from `main`, where the debounce and watcher plumbing buried the
/// behavior itself. It takes the same `RunParams` as a one-shot build, so
/// watch mode and `fh <build>` cannot drift apart in what they send. It does
/// not return: it loops until the user interrupts a run, then exits.
async fn run_watch(p: RunParams<'_>) -> ! {
    let RunParams {
        host,
        token,
        project_name,
        project_dir,
        command,
        outputs,
        out_dir,
        template,
        no_cache,
        run_env,
        run_toolchain,
        tty,
        forwards,
        compression,
        verbose,
        tls_config,
        telemetry,
    } = p;

    fn is_mutation_event(event: &notify::Event) -> bool {
        matches!(
            event.kind,
            notify::EventKind::Create(_)
                | notify::EventKind::Modify(notify::event::ModifyKind::Data(_))
                | notify::EventKind::Modify(notify::event::ModifyKind::Name(_))
                | notify::EventKind::Modify(notify::event::ModifyKind::Any)
                | notify::EventKind::Remove(_)
        )
    }

    println!(
        "👁️  farhand watch mode active for: {}",
        project_dir.display()
    );
    println!("   Command: {:?}", command);
    println!("   Press Ctrl+C to exit.\n");

    let initial_code = Box::pin(run_build(RunParams {
        host,
        token,
        project_name,
        project_dir,
        command,
        outputs: outputs.clone(),
        out_dir,
        template: template.clone(),
        no_cache,
        run_env: run_env.clone(),
        run_toolchain: run_toolchain.clone(),
        tty,
        forwards,
        compression: compression.clone(),
        verbose,
        tls_config,
        telemetry,
    }))
    .await;

    if let Ok(130) = initial_code {
        println!("\n👋 Watch mode stopped by user.");
        exit(0);
    }

    println!("\n👁️  Watching for changes... (debounce: 150ms)");

    // Bounded channel: an editor firing thousands of events fills at most
    // 256 slots (older events are dropped — the debounce tick coalesces
    // what matters into a single rebuild).
    let (tx, mut rx) = tokio::sync::mpsc::channel::<notify::Event>(256);
    let mut watcher = match RecommendedWatcher::new(
        move |res: notify::Result<notify::Event>| {
            if let Ok(event) = res {
                // Drop-on-full: coalescing happens at the debounce tick.
                let _ = tx.try_send(event);
            }
        },
        NotifyConfig::default(),
    ) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("Error initializing file watcher: {}", e);
            exit(EXIT_INFRA_ERROR);
        }
    };

    if let Err(e) = watcher.watch(project_dir, RecursiveMode::Recursive) {
        eprintln!("Error watching directory {}: {}", project_dir.display(), e);
        exit(EXIT_INFRA_ERROR);
    }

    // Drain any filesystem events generated during initial scan/build
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    while rx.try_recv().is_ok() {}

    loop {
        let first_event = tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("\n👋 Watch mode stopped by user.");
                break;
            }
            evt = rx.recv() => {
                match evt {
                    Some(e) => e,
                    None => break,
                }
            }
        };

        if !is_mutation_event(&first_event) {
            continue;
        }

        let mut relevant = first_event
            .paths
            .iter()
            .any(|p| !should_ignore_path(p, project_dir));

        let debounce_dur = std::time::Duration::from_millis(150);
        let deadline = tokio::time::Instant::now() + debounce_dur;

        let mut interrupted = false;
        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    println!("\n👋 Watch mode stopped by user.");
                    interrupted = true;
                    break;
                }
                next = rx.recv() => {
                    if let Some(event) = next {
                        if is_mutation_event(&event) && event.paths.iter().any(|p| !should_ignore_path(p, project_dir)) {
                            relevant = true;
                        }
                    } else {
                        break;
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    break;
                }
            }
        }

        if interrupted {
            break;
        }

        if relevant {
            // Drain any extra pending events before rebuilding
            while rx.try_recv().is_ok() {}

            println!("\n🔄 Change detected, syncing and rebuilding...");
            let code = Box::pin(run_build(RunParams {
                host,
                token,
                project_name,
                project_dir,
                command,
                outputs: outputs.clone(),
                out_dir,
                template: template.clone(),
                no_cache,
                run_env: run_env.clone(),
                run_toolchain: run_toolchain.clone(),
                tty,
                forwards,
                compression: compression.clone(),
                verbose,
                tls_config,
                telemetry,
            }))
            .await;

            if let Ok(130) = code {
                println!("\n👋 Watch mode stopped by user.");
                break;
            }

            // Settle and drain events triggered by local artifact extraction or touch
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            while rx.try_recv().is_ok() {}

            println!("\n👁️  Watching for changes...");
        }
    }
    exit(0);
}

/// Stack for the thread that runs the client.
///
/// Windows gives the main thread a 1 MiB stack; Linux gives it 8 MiB. The
/// client needs more than 1 MiB before it does any work at all — an
/// unoptimized build's async state machines, the clap command tree, and the
/// scan/upload/download futures all live in the same frame chain — so on
/// Windows `fh` aborted with `STATUS_STACK_OVERFLOW` (0xC00000FD) before
/// printing its version. Rather than depend on whatever the platform hands
/// the main thread, the work runs on a thread whose stack we choose.
const MAIN_STACK_SIZE: usize = 16 * 1024 * 1024;

fn main() {
    match std::thread::Builder::new()
        .name("farhand-cli".to_string())
        .stack_size(MAIN_STACK_SIZE)
        .spawn(run_cli)
    {
        Ok(handle) => {
            // Every exit path inside `run_cli` calls `std::process::exit`
            // explicitly, so a clean join here is the exceptional case.
            if handle.join().is_err() {
                std::process::exit(EXIT_INFRA_ERROR);
            }
        }
        Err(e) => {
            eprintln!("Error: could not start farhand ({e}).");
            std::process::exit(EXIT_INFRA_ERROR);
        }
    }
}

#[tokio::main]
async fn run_cli() {
    let raw_args: Vec<String> = std::env::args().collect();
    if raw_args.len() > 1 && raw_args[1] == "__test_echo" {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stdin = tokio::io::stdin();
        let mut stdout = tokio::io::stdout();
        let mut buf = [0u8; 4096];
        while let Ok(n) = stdin.read(&mut buf).await {
            if n == 0 {
                break;
            }
            let _ = stdout.write_all(&buf[..n]).await;
            let _ = stdout.flush().await;
        }
        std::process::exit(0);
    }
    if raw_args.len() > 1 && raw_args[1] == "__test_sleep" {
        let ms: u64 = raw_args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1000);
        tokio::time::sleep(tokio::time::Duration::from_millis(ms)).await;
        std::process::exit(0);
    }

    let cli = Cli::parse();
    setup_tracing(&cli.log_level, &cli.log_format);
    let mut telemetry = Telemetry::default();

    let project_dir = match cli.dir.canonicalize() {
        Ok(d) => d,
        Err(e) => {
            eprintln!(
                "Error: invalid project directory '{}': {}",
                cli.dir.display(),
                e
            );
            exit(EXIT_INFRA_ERROR);
        }
    };

    // Check for local init subcommand that does not require remote connection
    if let Some(Subcommands::Init {
        path,
        host,
        token,
        token_env,
        name,
        template,
        with_template,
        force,
    }) = &cli.subcommand
    {
        let mut opts = fh::InitOptions {
            path: path.clone(),
            host: host.clone(),
            token: token.clone(),
            name: name.clone(),
            template: template.clone(),
            with_template: *with_template,
            force: *force,
        };
        if *token_env {
            // --token-env opts into the environment-interpolation form; the
            // plaintext --token path still works but warns (see init.rs).
            opts.token = None;
        }

        match fh::init_project(&opts) {
            Ok(res) => {
                println!(
                    "✓ Initialized Farhand configuration at {}",
                    res.config_path.display()
                );
                if let Some(t_path) = res.template_path {
                    println!("✓ Initialized project template at {}", t_path.display());
                }
                println!(
                    "\nProject '{}' is configured for Farhand!",
                    res.project_name
                );
                if let Some(t) = res.detected_template {
                    println!("  • Template preset: {}", t);
                }
                println!("  • To run a remote build: fh <command>");
                exit(0);
            }
            Err(e) => {
                eprintln!("Error: {}", e);
                exit(1);
            }
        }
    }

    // Check for local template subcommands that do not require remote connection
    if let Some(Subcommands::Templates { ref action }) = cli.subcommand {
        match action {
            TemplateAction::List => {
                let available = templates::load_templates(Some(&project_dir));
                let mut names: Vec<_> = available.keys().collect();
                names.sort();
                println!("{:<12} {:<10} {:<40}", "NAME", "SOURCE", "DESCRIPTION");
                println!("{:-<12} {:-<10} {:-<40}", "", "", "");
                for name in names {
                    let t = &available[name];
                    println!(
                        "{:<12} {:<10} {}",
                        t.template.name, t.source, t.template.description
                    );
                }
                exit(0);
            }
            TemplateAction::Show { name } => {
                let available = templates::load_templates(Some(&project_dir));
                if let Some(t) = available.get(name) {
                    print!("{}", t.raw_yaml);
                    exit(0);
                } else {
                    eprintln!("Error: template '{}' not found", name);
                    exit(EXIT_INFRA_ERROR);
                }
            }
            TemplateAction::Init { name } => {
                let available = templates::load_templates(Some(&project_dir));
                let yaml_content = if let Some(t) = available.get(name) {
                    t.raw_yaml.clone()
                } else {
                    format!(
                        "name: {}\ndescription: Custom {} build toolchain\nmatch:\n  anyFile:\n    - {}.json\noutputs:\n  - dist\nignoreExtra: []\n",
                        name, name, name
                    )
                };
                match templates::save_template(Some(&project_dir), name, &yaml_content, "project") {
                    Ok(p) => {
                        println!("Initialized template at {}", p.display());
                        exit(0);
                    }
                    Err(e) => {
                        eprintln!("Error saving template: {}", e);
                        exit(EXIT_INFRA_ERROR);
                    }
                }
            }
            TemplateAction::Push { .. } => {}
        }
    }

    // Load configuration from .farhand.yaml or explicit --config path
    let (config_path, is_explicit_config) = match &cli.config {
        Some(p) => (p.clone(), true),
        None => (project_dir.join(".farhand.yaml"), false),
    };

    let cfg = match config::load_config_optional(&config_path, is_explicit_config) {
        Ok(c) => c.unwrap_or_default(),
        Err(e) => {
            eprintln!("Error loading configuration: {}", e);
            exit(EXIT_INFRA_ERROR);
        }
    };

    let verbose = cli.verbose || cfg.verbose;

    // Validate flag payloads that used to be silently ignored.
    for spec in &cli.forward {
        if let Err(e) = fh::parse_forward_spec(spec) {
            eprintln!("Error: invalid --forward '{}': {}", spec, e);
            exit(EXIT_INFRA_ERROR);
        }
    }
    if let Some(compression) = &cli.compression {
        let lowered = compression.to_ascii_lowercase();
        if !matches!(
            lowered.as_str(),
            "zstd" | "gzip" | "gz" | "none" | "plain" | "tar"
        ) {
            eprintln!(
                "Error: unknown --compression '{}'. Allowed values: zstd, gzip, none",
                compression
            );
            exit(EXIT_INFRA_ERROR);
        }
    }

    // --print-env is a dry-run: show exactly which variable NAMES would be
    // forwarded under the current policy (flags + config), then exit.
    if cli.print_env {
        let names = fh::envfilter::forwarded_env_names(
            std::env::vars(),
            cli.no_env,
            cfg.forward_env,
            &cfg.env,
            &cli.env,
        );
        if names.is_empty() {
            println!("No environment variables would be forwarded.");
        } else {
            println!(
                "{} environment variable(s) would be forwarded (names only):",
                names.len()
            );
            for name in &names {
                println!("  {name}");
            }
            println!(
                "\nFiltered out: session/OS vars, FARHAND_*, infrastructure credential \
                 prefixes (AWS_, GITHUB_, ...) and credential suffixes (*_TOKEN, *_SECRET, ...). \
                 Use -e VAR=... to forward a specific variable explicitly."
            );
        }
        exit(0);
    }

    // Precedence: CLI Flags > Environment Variables > Multi-Agent Pool > Config Host > Defaults
    let (host, host_token, pool_tls) = if let Some(h) = cli.host {
        (h, None, None)
    } else if !cfg.agents.is_empty() {
        let tag = cli.agent_tag.as_deref().or(cfg.agent_tag.as_deref());
        match fh::select_best_agent(&cfg.agents, tag, verbose).await {
            Ok(selected) => (selected.host, selected.token, selected.tls),
            Err(e) => {
                eprintln!("Error: failed to select agent from pool: {}", e);
                exit(EXIT_INFRA_ERROR);
            }
        }
    } else if let Some(h) = cfg.host.clone() {
        (h, None, None)
    } else {
        eprintln!("Error: agent host address is required (use --host, FARHAND_HOST env, or configure in .farhand.yaml)");
        exit(EXIT_INFRA_ERROR);
    };

    let effective_tls_setting = pool_tls.as_ref().or(cfg.tls.as_ref());
    let tls_config = fh::resolve_tls_config(
        effective_tls_setting,
        cli.tls,
        cli.tls_ca.as_deref(),
        cli.tls_fingerprint.as_deref(),
        cli.tls_insecure,
        cli.tls_cert.as_deref(),
        cli.tls_key.as_deref(),
    );

    let run_toolchain = collect_toolchain(&cfg.toolchain, &cli.toolchain);

    let insecure_skip_token = cli.insecure_skip_token || cfg.insecure_skip_token;

    let token = cli.token.or(host_token).or(cfg.token.clone()).unwrap_or_else(|| {
        if insecure_skip_token {
            String::new()
        } else {
            eprintln!("Error: shared auth token is required (use --token, FARHAND_TOKEN env, or configure in .farhand.yaml)");
            exit(EXIT_INFRA_ERROR);
        }
    });

    let base_project_name = cli.name.or(cfg.name.clone()).unwrap_or_else(|| {
        project_dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unnamed_project".into())
    });

    let branch = if cli.no_branch_scope {
        None
    } else if let Some(b) = cli.branch {
        Some(b)
    } else {
        detect_git_branch(&project_dir)
    };

    let project_name = match branch {
        Some(ref b) if b != "main" && b != "master" => {
            format!("{}__{}", base_project_name, sanitize_branch_name(b))
        }
        _ => base_project_name.clone(),
    };

    // Handle Clean subcommand
    if let Some(Subcommands::Clean {
        name,
        all_branches,
        caches_only,
    }) = cli.subcommand
    {
        let proj = name.unwrap_or(project_name);

        match fh::clean_workspace(
            &host,
            &token,
            &proj,
            all_branches,
            caches_only,
            tls_config.as_ref(),
        )
        .await
        {
            Ok(resp) => {
                if cli.log_format == "json" {
                    if let Ok(json) = serde_json::to_string_pretty(&resp) {
                        println!("{}", json);
                    }
                } else {
                    println!("[clean] {}", resp.message);
                    if resp.bytes_freed > 0 {
                        println!(
                            "[clean] Space freed: {}",
                            fh::format_bytes(resp.bytes_freed)
                        );
                    }
                }
                exit(0);
            }
            Err(e) => {
                eprintln!("Error cleaning workspace: {}", e);
                exit(EXIT_INFRA_ERROR);
            }
        }
    }

    // Handle History subcommand
    if let Some(Subcommands::History { name, limit }) = cli.subcommand {
        let proj = name.unwrap_or(project_name);

        match fh::query_history(&host, &token, &proj, limit, tls_config.as_ref()).await {
            Ok(resp) => {
                if cli.log_format == "json" {
                    if let Ok(json) = serde_json::to_string_pretty(&resp) {
                        println!("{}", json);
                    }
                } else {
                    fh::render_history_table(&resp);
                }
                exit(0);
            }
            Err(e) => {
                eprintln!("Error querying history: {}", e);
                exit(EXIT_INFRA_ERROR);
            }
        }
    }

    // Handle Top subcommand
    if let Some(Subcommands::Top {
        agent,
        once,
        interval,
    }) = cli.subcommand
    {
        let target_host = agent.unwrap_or(host);
        match fh::run_top(&target_host, &token, tls_config.as_ref(), once, interval).await {
            Ok(_) => exit(0),
            Err(e) => {
                eprintln!("Error in top dashboard: {}", e);
                exit(EXIT_INFRA_ERROR);
            }
        }
    }

    // Handle Agent subcommand
    if let Some(Subcommands::Agent { action }) = cli.subcommand {
        match action {
            AgentAction::Info { agent, json } => {
                let target_host = agent.unwrap_or(host);
                match fh::run_agent_info(&target_host, &token, tls_config.as_ref(), json).await {
                    Ok(_) => exit(0),
                    Err(e) => {
                        eprintln!("Error querying agent info: {}", e);
                        exit(EXIT_INFRA_ERROR);
                    }
                }
            }
        }
    }

    // Handle Lsp subcommand
    if let Some(Subcommands::Lsp {
        agent,
        no_sync,
        no_save_sync,
        command,
    }) = cli.subcommand
    {
        if command.is_empty() {
            eprintln!("Error: no LSP command specified. Usage: fh lsp -- <LSP_BINARY> [ARGS]...");
            exit(EXIT_INFRA_ERROR);
        }
        let target_host = agent.unwrap_or(host);
        match fh::run_lsp(
            &target_host,
            &token,
            &project_name,
            &project_dir,
            &command,
            no_sync,
            no_save_sync,
            tls_config.as_ref(),
        )
        .await
        {
            Ok(code) => exit(code),
            Err(e) => {
                eprintln!("Error in LSP bridge: {}", e);
                exit(EXIT_INFRA_ERROR);
            }
        }
    }

    // Handle remote template subcommands (Push)
    if let Some(Subcommands::Templates {
        action: TemplateAction::Push { name, scope },
    }) = cli.subcommand
    {
        let available = templates::load_templates(Some(&project_dir));
        let t = match available.get(&name) {
            Some(t) => t,
            None => {
                eprintln!("Error: template '{}' not found locally", name);
                exit(EXIT_INFRA_ERROR);
            }
        };

        let mut stream = match fh::connect_to_agent(&host, tls_config.as_ref()).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Error: unable to reach agent at {}: {}", host, e);
                exit(EXIT_INFRA_ERROR);
            }
        };

        // No compression offer and no queue wait: one-shot admin frames must
        // be answered immediately by the agent.
        perform_handshake(&mut stream, &token, &project_name, None, false).await;

        let put = PutTemplatePayload {
            name: name.clone(),
            yaml: t.raw_yaml.clone(),
            scope: scope.clone(),
        };
        if let Err(e) = write_json_frame(&mut stream, MsgType::PutTemplate, &put).await {
            eprintln!("Error: failed to send PUT_TEMPLATE: {}", e);
            exit(EXIT_INFRA_ERROR);
        }

        let (msg_type, payload) = match read_frame(&mut stream).await {
            Ok(f) => f,
            Err(e) => {
                eprintln!("Error: failed to read response for PUT_TEMPLATE: {}", e);
                exit(EXIT_INFRA_ERROR);
            }
        };
        if msg_type != MsgType::HelloAck {
            eprintln!(
                "Protocol error: expected response frame, got {:?}",
                msg_type
            );
            exit(EXIT_INFRA_ERROR);
        }
        let ack: HelloAckPayload = match decode_json(&payload) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("Error decoding response: {}", e);
                exit(EXIT_INFRA_ERROR);
            }
        };
        if ack.ok {
            println!(
                "Template '{}' uploaded successfully to remote agent (scope: {})",
                name, scope
            );
            exit(0);
        } else {
            eprintln!(
                "Failed to upload template: {}",
                ack.error.as_deref().unwrap_or("unknown error")
            );
            exit(EXIT_INFRA_ERROR);
        }
    }

    let is_watch = cli.watch || matches!(cli.subcommand, Some(Subcommands::Watch { .. }));
    let is_shell = matches!(cli.subcommand, Some(Subcommands::Shell { .. }));
    let is_exec = matches!(cli.subcommand, Some(Subcommands::Exec { .. }));

    let effective_command = match &cli.subcommand {
        Some(Subcommands::Watch { command }) if !command.is_empty() => command.clone(),
        Some(Subcommands::Shell { shell, .. }) => {
            if let Some(sh) = shell {
                vec![sh.clone()]
            } else {
                vec!["$SHELL".to_string()]
            }
        }
        Some(Subcommands::Exec { command, .. }) => command.clone(),
        _ => cli.command.clone(),
    };

    if effective_command.is_empty() {
        eprintln!("Error: no remote command specified. Usage: fh [OPTIONS] <COMMAND>... or fh watch <COMMAND>... or fh shell");
        exit(EXIT_INFRA_ERROR);
    }

    let resolved_cfg_outputs = cfg.resolved_outputs();
    let outputs = if is_shell || is_exec || cli.no_output {
        None
    } else if !cli.output.is_empty() {
        Some(cli.output)
    } else if !resolved_cfg_outputs.is_empty() {
        Some(resolved_cfg_outputs)
    } else {
        None
    };

    let out_dir = cli
        .out_dir
        .or_else(|| cfg.out_dir.map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("./farhand-out"));

    let template = if is_shell || is_exec {
        None
    } else {
        cli.template.or(cfg.template)
    };
    let no_cache = cli.no_cache || cfg.no_cache;
    let effective_tty = if is_shell {
        true
    } else if let Some(Subcommands::Exec { tty, .. }) = &cli.subcommand {
        *tty || cli.tty || cfg.tty
    } else {
        cli.tty || cfg.tty
    };
    let effective_forwards = if !cli.forward.is_empty() {
        cli.forward
    } else {
        cfg.forward
    };
    let mut run_env = collect_forward_env(cli.no_env, cfg.forward_env, &cfg.env, &cli.env);
    if effective_tty {
        let env_map = run_env.get_or_insert_with(std::collections::HashMap::new);
        if !env_map.contains_key("TERM") {
            let term = std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".to_string());
            env_map.insert("TERM".to_string(), term);
        }
    }

    let effective_compression = cli.compression.or(cfg.compression);

    if is_watch {
        Box::pin(run_watch(RunParams {
            host: &host,
            token: &token,
            project_name: &project_name,
            project_dir: &project_dir,
            command: &effective_command,
            outputs: outputs.clone(),
            out_dir: &out_dir,
            template: template.clone(),
            no_cache,
            run_env: run_env.clone(),
            run_toolchain: run_toolchain.clone(),
            tty: effective_tty,
            forwards: &effective_forwards,
            compression: effective_compression.clone(),
            verbose,
            tls_config: tls_config.as_ref(),
            telemetry: &mut telemetry,
        }))
        .await;
    }

    let exit_code = match Box::pin(run_build(RunParams {
        host: &host,
        token: &token,
        project_name: &project_name,
        project_dir: &project_dir,
        command: &effective_command,
        outputs,
        out_dir: &out_dir,
        template,
        no_cache,
        run_env,
        run_toolchain,
        tty: effective_tty,
        forwards: &effective_forwards,
        compression: effective_compression,
        verbose,
        tls_config: tls_config.as_ref(),
        telemetry: &mut telemetry,
    }))
    .await
    {
        Ok(code) => code,
        Err(e) => {
            eprintln!("Build execution error: {}", e);
            EXIT_INFRA_ERROR
        }
    };

    if verbose {
        telemetry.print_summary(&project_name, &host);
    }

    exit(exit_code);
}
