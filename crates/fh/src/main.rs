use clap::{Parser, Subcommand};
use protocol::{
    decode_json, read_frame, write_frame, write_json_frame, FileEntry, HelloAckPayload,
    HelloPayload, LogPayload, ManifestPayload, MsgType, NeedPayload, PutTemplatePayload,
    ResultPayload, RunPayload, CURRENT_PROTOCOL_VERSION,
};
use std::io::Write;
use std::path::PathBuf;
use std::process::exit;
use std::time::Instant;
use tokio::net::TcpStream;

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

    #[arg(long = "output", action = clap::ArgAction::Append, help = "Explicit path(s) to fetch back after a successful run (repeatable)")]
    output: Vec<String>,

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

    #[arg(trailing_var_arg = true, help = "Command to run remotely")]
    command: Vec<String>,
}

#[derive(Subcommand, Debug)]
enum Subcommands {
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
    let (host, host_token) = if let Some(h) = cli.host {
        (h, None)
    } else if !cfg.agents.is_empty() {
        let tag = cli.agent_tag.as_deref().or(cfg.agent_tag.as_deref());
        match fh::select_best_agent(&cfg.agents, tag, verbose).await {
            Ok(selected) => (selected.host, selected.token),
            Err(e) => {
                eprintln!("Error: failed to select agent from pool: {}", e);
                exit(EXIT_INFRA_ERROR);
            }
        }
    } else if let Some(h) = cfg.host {
        (h, None)
    } else {
        eprintln!("Error: agent host address is required (use --host, FARHAND_HOST env, or configure in .farhand.yaml)");
        exit(EXIT_INFRA_ERROR);
    };

    let insecure_skip_token = cli.insecure_skip_token || cfg.insecure_skip_token;

    let token = cli.token.or(host_token).or(cfg.token).unwrap_or_else(|| {
        if insecure_skip_token {
            String::new()
        } else {
            eprintln!("Error: shared auth token is required (use --token, FARHAND_TOKEN env, or configure in .farhand.yaml)");
            exit(EXIT_INFRA_ERROR);
        }
    });

    let base_project_name = cli.name.or(cfg.name).unwrap_or_else(|| {
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

        match fh::clean_workspace(&host, &token, &proj, all_branches, caches_only).await {
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

        match fh::query_history(&host, &token, &proj, limit).await {
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

        let mut stream = match TcpStream::connect(&host).await {
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

    if cli.command.is_empty() {
        eprintln!("Error: no remote command specified. Usage: fh [OPTIONS] <COMMAND>...");
        exit(EXIT_INFRA_ERROR);
    }

    let outputs = if !cli.output.is_empty() {
        Some(cli.output)
    } else if !cfg.outputs.is_empty() {
        Some(cfg.outputs)
    } else {
        None
    };

    let out_dir = cli
        .out_dir
        .or_else(|| cfg.out_dir.map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("./farhand-out"));

    let template = cli.template.or(cfg.template);
    let no_cache = cli.no_cache || cfg.no_cache;

    if verbose {
        println!("=== Farhand Remote Runner ===");
        println!("Connecting to agent at: {}", host);
        println!("Project: {} ({})", project_name, project_dir.display());
        println!("Remote command: {:?}", cli.command);
    }

    // 1. Connect TCP to agent
    let mut stream = match TcpStream::connect(&host).await {
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
    let hello = HelloPayload {
        token,
        project: project_name.clone(),
        protocol_version: CURRENT_PROTOCOL_VERSION,
    };

    if let Err(e) = write_json_frame(&mut stream, MsgType::Hello, &hello).await {
        eprintln!("Error: failed to send HELLO handshake: {}", e);
        exit(EXIT_INFRA_ERROR);
    }

    // Read HELLO_ACK
    let (msg_type, payload) = match read_frame(&mut stream).await {
        Ok(f) => f,
        Err(e) => {
            eprintln!("Error: failed to read HELLO_ACK: {}", e);
            exit(EXIT_INFRA_ERROR);
        }
    };

    if msg_type != MsgType::HelloAck {
        eprintln!(
            "Protocol error: expected HELLO_ACK, received {:?}",
            msg_type
        );
        exit(EXIT_INFRA_ERROR);
    }

    let ack: HelloAckPayload = match decode_json(&payload) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("Error: invalid HELLO_ACK payload: {}", e);
            exit(EXIT_INFRA_ERROR);
        }
    };

    if !ack.ok {
        eprintln!(
            "Authentication failed: {}",
            ack.error.as_deref().unwrap_or("rejected by agent")
        );
        exit(EXIT_INFRA_ERROR);
    }

    // 3. Scan local files & Build MANIFEST
    let scan_start = Instant::now();
    let scanned_files = match fileset::scan(&project_dir, &[]) {
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

    if verbose {
        println!(
            "Scanned {} local files in {:?}",
            manifest.files.len(),
            telemetry.scan_duration
        );
    }

    // 4. Send MANIFEST frame
    if let Err(e) = write_json_frame(&mut stream, MsgType::Manifest, &manifest).await {
        eprintln!("Error: failed to send MANIFEST frame: {}", e);
        exit(EXIT_INFRA_ERROR);
    }

    // 5. Receive NEED frame (handling optional QUEUED frames first)
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

    // 6. Selective Delta Pack & Upload (FILES frame)
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
        let tar_gz = match fileset::pack_tar(&project_dir, &need.want) {
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

    // 7. Send RUN frame
    let run_env = collect_forward_env(cli.no_env, cfg.forward_env, &cfg.env, &cli.env);
    if verbose {
        if let Some(ref e) = run_env {
            println!(
                "[Environment] Forwarding {} environment variable(s) to remote agent",
                e.len()
            );
        } else {
            println!("[Environment] Environment variable forwarding is disabled");
        }
    }

    let run = RunPayload {
        argv: cli.command,
        outputs,
        cwd: None,
        template,
        no_cache,
        env: run_env,
    };

    let remote_start = Instant::now();
    if let Err(e) = write_json_frame(&mut stream, MsgType::Run, &run).await {
        eprintln!("Error: failed to send RUN frame: {}", e);
        exit(EXIT_INFRA_ERROR);
    }

    // 8. Receive streamed LOG, RESULT, and optional ARTIFACTS frames
    let mut exit_code = 1;
    let mut stdout = std::io::stdout();
    let mut stderr = std::io::stderr();
    let mut received_result = false;

    loop {
        let (msg_type, payload) = match read_frame(&mut stream).await {
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
            MsgType::Result => {
                telemetry.remote_duration = remote_start.elapsed();
                if let Ok(res) = decode_json::<ResultPayload>(&payload) {
                    exit_code = res.exit_code;
                    received_result = true;
                    if let Some(err_msg) = res.error {
                        eprintln!("Remote error: {}", err_msg);
                    }
                    if exit_code != 0 {
                        // On command failure, no artifacts will be sent
                        break;
                    }
                } else {
                    eprintln!("Protocol error: failed to decode RESULT payload");
                    exit(EXIT_INFRA_ERROR);
                }
            }
            MsgType::Artifacts => {
                let extract_start = Instant::now();
                if let Err(e) = fileset::unpack_tar(&out_dir, &payload) {
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
            other => {
                if verbose {
                    println!("[farhand] Received control frame: {:?}", other);
                }
            }
        }
    }

    if verbose {
        telemetry.print_summary(&project_name, &host);
    }

    exit(exit_code);
}
