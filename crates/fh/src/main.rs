use clap::{Parser, Subcommand};
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
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

const EXIT_INFRA_ERROR: i32 = 125;

const FILTERED_ENV_VARS: &[&str] = &[
    // Core OS / User session
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "PWD",
    "OLDPWD",
    "TMPDIR",
    "TEMP",
    "TMP",
    "TERM",
    "TERMCAP",
    "SHLVL",
    "_",
    // Farhand internals
    "FARHAND_HOST",
    "FARHAND_TOKEN",
    "FARHAND_WORKDIR",
    "FARHAND_LISTEN",
    "FARHAND_MAX_DISK_GB",
    "FARHAND_WORKSPACE_TTL_DAYS",
    // SSH & Terminal session
    "SSH_AUTH_SOCK",
    "SSH_AGENT_PID",
    "SSH_CONNECTION",
    "SSH_CLIENT",
    "SSH_TTY",
    // GUI / Desktop environments
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "XAUTHORITY",
    "XDG_RUNTIME_DIR",
    "XDG_SESSION_ID",
    "XDG_DATA_DIRS",
    "XDG_CONFIG_DIRS",
    "XDG_STATE_HOME",
    "XDG_CACHE_HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    // Editor / IDE specifics
    "VSCODE_INJECTION",
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "COLORTERM",
    "ANTIGRAVITY_SOURCE_METADATA",
];

fn collect_forward_env(
    no_env_flag: bool,
    config_forward_env: bool,
    config_env: &std::collections::HashMap<String, String>,
    cli_env: &[String],
) -> Option<std::collections::HashMap<String, String>> {
    let mut map = std::collections::HashMap::new();

    // 1. If ambient forwarding is enabled (default), collect non-filtered local env vars
    if !no_env_flag && config_forward_env {
        for (k, v) in std::env::vars() {
            if !FILTERED_ENV_VARS.contains(&k.as_str()) && !k.starts_with("FARHAND_") {
                map.insert(k, v);
            }
        }
    }

    // 2. Overlay environment variables defined in .farhand.yaml
    for (k, v) in config_env {
        map.insert(k.clone(), v.clone());
    }

    // 3. Overlay explicit CLI flags: -e KEY=VAL or -e KEY (takes value from current local env)
    for entry in cli_env {
        if let Some((k, v)) = entry.split_once('=') {
            map.insert(k.to_string(), v.to_string());
        } else if let Ok(v) = std::env::var(entry) {
            map.insert(entry.clone(), v);
        }
    }

    if map.is_empty() {
        None
    } else {
        Some(map)
    }
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

#[derive(Parser, Debug)]
#[command(
    name = "fh",
    version,
    about = "Farhand client: offload build/test execution to remote agent"
)]
struct Cli {
    #[command(subcommand)]
    subcommand: Option<Subcommands>,

    #[arg(
        long,
        env = "FARHAND_HOST",
        help = "Agent address, host:port (required, or from config)"
    )]
    host: Option<String>,

    #[arg(
        long,
        env = "FARHAND_TOKEN",
        help = "Shared authentication token (required, or from config)"
    )]
    token: Option<String>,

    #[arg(long, default_value = ".", help = "Local directory to sync")]
    dir: PathBuf,

    #[arg(
        long,
        help = "Project name / workspace key (default: local dir basename)"
    )]
    name: Option<String>,

    #[arg(long, help = "Path to configuration file (default: ./.farhand.yaml)")]
    config: Option<PathBuf>,

    #[arg(long, help = "Allow connecting to an agent with no token configured")]
    insecure_skip_token: bool,

    #[arg(
        short,
        long,
        help = "Print detailed sync, timing, and telemetry statistics"
    )]
    verbose: bool,

    #[arg(short = 'o', long = "output", action = clap::ArgAction::Append, help = "Explicit path(s) to fetch back after a successful run (repeatable)")]
    output: Vec<String>,

    #[arg(long = "no-output", help = "Disable artifact retrieval for this run")]
    no_output: bool,

    #[arg(
        long = "out-dir",
        help = "Local directory to extract artifacts into (default: ./farhand-out)"
    )]
    out_dir: Option<PathBuf>,

    #[arg(long, help = "Force specific template by name")]
    template: Option<String>,

    #[arg(long, help = "Pin to agent with tag (multi-agent mode)")]
    agent_tag: Option<String>,

    #[arg(long, help = "Bypass lockfile dependency caching hooks")]
    no_cache: bool,

    #[arg(
        long,
        help = "Explicit branch name for workspace isolation (defaults to current git branch)"
    )]
    branch: Option<String>,

    #[arg(long, help = "Disable automatic git branch workspace scoping")]
    no_branch_scope: bool,

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
        long = "no-env",
        action = clap::ArgAction::SetTrue,
        help = "Disable forwarding local environment variables to the remote agent (forwarding is ON by default)"
    )]
    no_env: bool,

    #[arg(
        short = 'e',
        long = "env",
        action = clap::ArgAction::Append,
        help = "Explicit environment variable to pass to remote command, in KEY=VALUE or KEY format (repeatable)"
    )]
    env: Vec<String>,

    #[arg(
        short = 't',
        long = "tty",
        help = "Allocate a pseudo-terminal (PTY) on the remote agent for interactive commands"
    )]
    tty: bool,

    #[arg(
        short = 'L',
        long = "forward",
        action = clap::ArgAction::Append,
        help = "Forward local port to remote agent port, formatted LOCAL:REMOTE (e.g. 3000:3000, repeatable)"
    )]
    forward: Vec<String>,

    #[arg(
        long = "watch",
        help = "Watch local files and re-trigger remote build continuously on file change"
    )]
    watch: bool,

    #[arg(
        long = "compression",
        help = "Wire compression algorithm ('zstd', 'gzip', or 'none')"
    )]
    compression: Option<String>,

    #[arg(
        long = "tls",
        action = clap::ArgAction::SetTrue,
        help = "Enable TLS encryption for connection to agent"
    )]
    tls: bool,

    #[arg(
        long = "tls-ca",
        help = "Path to custom CA certificate (PEM) to verify agent TLS certificate"
    )]
    tls_ca: Option<PathBuf>,

    #[arg(
        long = "tls-fingerprint",
        help = "Expected SHA-256 fingerprint of the agent TLS certificate"
    )]
    tls_fingerprint: Option<String>,

    #[arg(
        long = "tls-insecure",
        action = clap::ArgAction::SetTrue,
        help = "Accept any server TLS certificate without validation (INSECURE)"
    )]
    tls_insecure: bool,

    #[arg(
        long = "tls-cert",
        help = "Path to client TLS certificate (PEM) for mTLS authentication"
    )]
    tls_cert: Option<PathBuf>,

    #[arg(
        long = "tls-key",
        help = "Path to client TLS private key (PEM) for mTLS authentication"
    )]
    tls_key: Option<PathBuf>,

    #[arg(
        short = 'T',
        long = "toolchain",
        action = clap::ArgAction::Append,
        help = "Declarative toolchain version override, e.g. -T rust=1.78.0 -T node=20 (repeatable)"
    )]
    toolchain: Vec<String>,

    #[arg(trailing_var_arg = true, help = "Command to run remotely")]
    command: Vec<String>,
}

#[derive(Subcommand, Debug)]
enum Subcommands {
    /// Initialize Farhand project configuration (.farhand.yaml) and templates
    Init {
        /// Target project directory (default: current directory)
        #[arg(default_value = ".")]
        path: PathBuf,

        /// Remote agent address (host:port)
        #[arg(long)]
        host: Option<String>,

        /// Remote authentication token
        #[arg(long)]
        token: Option<String>,

        /// Project name (default: directory name)
        #[arg(short, long)]
        name: Option<String>,

        /// Project template preset (rust, npm, go, python, maven, gradle, or custom)
        #[arg(short, long)]
        template: Option<String>,

        /// Also generate a project-level template in .farhand/templates/<name>.yaml
        #[arg(long)]
        with_template: bool,

        /// Overwrite existing configuration and template files if they exist
        #[arg(short, long)]
        force: bool,
    },
    /// Watch local files and continuously offload builds on change
    Watch {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Manage build and detection templates
    Templates {
        #[command(subcommand)]
        action: TemplateAction,
    },
    /// Query build and execution history from remote agent
    History {
        /// Project name to query (default: current project name)
        #[arg(long)]
        name: Option<String>,

        /// Maximum number of runs to display (default: 10)
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// Clean remote project workspaces or caches
    Clean {
        /// Specific project/branch name to clean (default: current project/branch)
        #[arg(long)]
        name: Option<String>,

        /// Clean all non-canonical branch workspaces for this project
        #[arg(long)]
        all_branches: bool,

        /// Only clean intermediate compiler/build caches (incremental caches, .cache)
        #[arg(long)]
        caches_only: bool,
    },
    /// Open an interactive shell inside the remote workspace
    Shell {
        /// Optional specific shell to launch (default: $SHELL or /bin/sh)
        #[arg(long)]
        shell: Option<String>,
        /// Do not sync local changes before opening shell
        #[arg(long)]
        no_sync: bool,
    },
    /// Run an ad-hoc command in the remote workspace without artifact sync or dependency hooks
    Exec {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
        /// Allocate pseudo-terminal (PTY) for interactive execution
        #[arg(short = 't', long = "tty")]
        tty: bool,
    },
    /// Monitor remote agent daemon activity and resource utilization
    Top {
        /// Remote agent address (host:port) to monitor
        #[arg(long)]
        agent: Option<String>,
        /// Print a single snapshot and exit instead of interactive dashboard
        #[arg(long)]
        once: bool,
        /// Update interval in seconds (default: 1)
        #[arg(short, long, default_value_t = 1)]
        interval: u64,
    },
    /// Inspect or manage remote agents
    Agent {
        #[command(subcommand)]
        action: AgentAction,
    },
    /// Offload Language Server Protocol (LSP) server to remote agent workspace
    Lsp {
        /// Remote agent address (host:port)
        #[arg(long)]
        agent: Option<String>,
        /// Bypass initial project sync before starting language server
        #[arg(long)]
        no_sync: bool,
        /// Disable auto-syncing changed files on textDocument/didSave
        #[arg(long)]
        no_save_sync: bool,
        /// LSP server binary and arguments to run remotely
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
}

#[derive(Subcommand, Debug)]
enum AgentAction {
    /// Display system specifications, resource usage, and active jobs for an agent
    Info {
        /// Remote agent address (host:port)
        #[arg(long)]
        agent: Option<String>,
        /// Output agent information as formatted JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug)]
enum TemplateAction {
    /// List all available templates and their resolution sources
    List,
    /// Display the raw YAML definition of a template
    Show {
        /// Name of the template to display
        name: String,
    },
    /// Initialize a template in .farhand/templates/<name>.yaml
    Init {
        /// Name of the template to initialize
        name: String,
    },
    /// Upload a template to the remote agent daemon
    Push {
        /// Name of the template to upload
        name: String,
        /// Scope on agent host ("project" or "user")
        #[arg(long, default_value = "project")]
        scope: String,
    },
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
async fn run_build(
    host: &str,
    token: &str,
    project_name: &str,
    project_dir: &Path,
    command: &[String],
    outputs: Option<Vec<String>>,
    out_dir: &Path,
    template: Option<String>,
    no_cache: bool,
    run_env: Option<HashMap<String, String>>,
    run_toolchain: Option<HashMap<String, String>>,
    tty: bool,
    forwards: &[String],
    compression: Option<String>,
    verbose: bool,
    tls_config: Option<&config::TlsConfig>,
    telemetry: &mut Telemetry,
) -> Result<i32, Box<dyn std::error::Error + Send + Sync>> {
    if verbose {
        println!("=== Farhand Remote Runner ===");
        println!("Connecting to agent at: {}", host);
        println!("Project: {} ({})", project_name, project_dir.display());
        println!("Remote command: {:?}", command);
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

    // 2. Handshake: Send HELLO
    let client_compressions = if let Some(c) = &compression {
        vec![c.to_string()]
    } else {
        vec!["zstd".to_string(), "gzip".to_string(), "none".to_string()]
    };

    let hello = HelloPayload {
        token: token.to_string(),
        project: project_name.to_string(),
        protocol_version: CURRENT_PROTOCOL_VERSION,
        compressions: Some(client_compressions),
    };

    if let Err(e) = write_json_frame(&mut stream, MsgType::Hello, &hello).await {
        eprintln!("Error: failed to send HELLO handshake: {}", e);
        exit(EXIT_INFRA_ERROR);
    }

    // Read HELLO_ACK or QUEUED
    let ack: HelloAckPayload = loop {
        let (msg_type, payload) = match read_frame(&mut stream).await {
            Ok(f) => f,
            Err(e) => {
                eprintln!("Error: failed to read HELLO_ACK: {}", e);
                exit(EXIT_INFRA_ERROR);
            }
        };

        match msg_type {
            MsgType::Queued => {
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

#[tokio::main]
async fn main() {
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
        name,
        template,
        with_template,
        force,
    }) = &cli.subcommand
    {
        let opts = fh::InitOptions {
            path: path.clone(),
            host: host.clone(),
            token: token.clone(),
            name: name.clone(),
            template: template.clone(),
            with_template: *with_template,
            force: *force,
        };

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

        let hello = HelloPayload {
            token,
            project: project_name,
            protocol_version: CURRENT_PROTOCOL_VERSION,
            compressions: None,
        };
        if let Err(e) = write_json_frame(&mut stream, MsgType::Hello, &hello).await {
            eprintln!("Error: failed to send HELLO: {}", e);
            exit(EXIT_INFRA_ERROR);
        }
        let (msg_type, payload) = match read_frame(&mut stream).await {
            Ok(f) => f,
            Err(e) => {
                eprintln!("Error: failed to read HELLO_ACK: {}", e);
                exit(EXIT_INFRA_ERROR);
            }
        };
        if msg_type != MsgType::HelloAck {
            eprintln!("Protocol error: expected HELLO_ACK, got {:?}", msg_type);
            exit(EXIT_INFRA_ERROR);
        }
        let ack: HelloAckPayload = match decode_json(&payload) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("Error: invalid HELLO_ACK: {}", e);
                exit(EXIT_INFRA_ERROR);
            }
        };
        if !ack.ok {
            eprintln!(
                "Authentication failed: {}",
                ack.error.as_deref().unwrap_or("rejected")
            );
            exit(EXIT_INFRA_ERROR);
        }

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
        println!(
            "👁️  farhand watch mode active for: {}",
            project_dir.display()
        );
        println!("   Command: {:?}", effective_command);
        println!("   Press Ctrl+C to exit.\n");

        let initial_code = run_build(
            &host,
            &token,
            &project_name,
            &project_dir,
            &effective_command,
            outputs.clone(),
            &out_dir,
            template.clone(),
            no_cache,
            run_env.clone(),
            run_toolchain.clone(),
            effective_tty,
            &effective_forwards,
            effective_compression.clone(),
            verbose,
            tls_config.as_ref(),
            &mut telemetry,
        )
        .await;

        if let Ok(130) = initial_code {
            println!("\n👋 Watch mode stopped by user.");
            exit(0);
        }

        println!("\n👁️  Watching for changes... (debounce: 150ms)");

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut watcher = match RecommendedWatcher::new(
            move |res: notify::Result<notify::Event>| {
                if let Ok(event) = res {
                    let _ = tx.send(event);
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

        if let Err(e) = watcher.watch(&project_dir, RecursiveMode::Recursive) {
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
                .any(|p| !should_ignore_path(p, &project_dir));

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
                            if is_mutation_event(&event) && event.paths.iter().any(|p| !should_ignore_path(p, &project_dir)) {
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
                let code = run_build(
                    &host,
                    &token,
                    &project_name,
                    &project_dir,
                    &effective_command,
                    outputs.clone(),
                    &out_dir,
                    template.clone(),
                    no_cache,
                    run_env.clone(),
                    run_toolchain.clone(),
                    effective_tty,
                    &effective_forwards,
                    effective_compression.clone(),
                    verbose,
                    tls_config.as_ref(),
                    &mut telemetry,
                )
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

    let exit_code = match run_build(
        &host,
        &token,
        &project_name,
        &project_dir,
        &effective_command,
        outputs,
        &out_dir,
        template,
        no_cache,
        run_env,
        run_toolchain,
        effective_tty,
        &effective_forwards,
        effective_compression,
        verbose,
        tls_config.as_ref(),
        &mut telemetry,
    )
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
