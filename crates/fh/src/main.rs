use clap::Parser;
use protocol::{
    decode_json, read_frame, write_frame, write_json_frame, FileEntry, HelloAckPayload,
    HelloPayload, LogPayload, ManifestPayload, MsgType, NeedPayload, ResultPayload, RunPayload,
    CURRENT_PROTOCOL_VERSION,
};
use std::io::Write;
use std::path::PathBuf;
use std::process::exit;
use std::time::Instant;
use tokio::net::TcpStream;

const EXIT_INFRA_ERROR: i32 = 125;

#[derive(Parser, Debug)]
#[command(name = "fh", about = "Farhand client: offload build/test execution to remote agent")]
struct Cli {
    #[arg(long, env = "FARHAND_HOST", help = "Agent address, host:port (required, or from config)")]
    host: Option<String>,

    #[arg(long, env = "FARHAND_TOKEN", help = "Shared authentication token (required, or from config)")]
    token: Option<String>,

    #[arg(long, default_value = ".", help = "Local directory to sync")]
    dir: PathBuf,

    #[arg(long, help = "Project name / workspace key (default: local dir basename)")]
    name: Option<String>,

    #[arg(long, help = "Path to configuration file (default: ./.farhand.yaml)")]
    config: Option<PathBuf>,

    #[arg(long, help = "Allow connecting to an agent with no token configured")]
    insecure_skip_token: bool,

    #[arg(short, long, help = "Print detailed sync, timing, and telemetry statistics")]
    verbose: bool,

    #[arg(long = "output", action = clap::ArgAction::Append, help = "Explicit path(s) to fetch back after a successful run (repeatable)")]
    output: Vec<String>,

    #[arg(long = "out-dir", help = "Local directory to extract artifacts into (default: ./farhand-out)")]
    out_dir: Option<PathBuf>,

    #[arg(long, help = "Force specific template by name")]
    template: Option<String>,

    #[arg(long, help = "Pin to agent with tag (multi-agent mode)")]
    agent_tag: Option<String>,

    #[arg(long, help = "Bypass lockfile dependency caching hooks")]
    no_cache: bool,

    #[arg(trailing_var_arg = true, required = true, help = "Command to run remotely")]
    command: Vec<String>,
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

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let mut telemetry = Telemetry::default();

    let project_dir = match cli.dir.canonicalize() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Error: invalid project directory '{}': {}", cli.dir.display(), e);
            exit(EXIT_INFRA_ERROR);
        }
    };

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

    // Precedence: CLI Flags > Environment Variables > Config File > Defaults
    let host = cli.host.or(cfg.host).unwrap_or_else(|| {
        eprintln!("Error: agent host address is required (use --host, FARHAND_HOST env, or configure in .farhand.yaml)");
        exit(EXIT_INFRA_ERROR);
    });

    let insecure_skip_token = cli.insecure_skip_token || cfg.insecure_skip_token;

    let token = cli.token.or(cfg.token).unwrap_or_else(|| {
        if insecure_skip_token {
            String::new()
        } else {
            eprintln!("Error: shared auth token is required (use --token, FARHAND_TOKEN env, or configure in .farhand.yaml)");
            exit(EXIT_INFRA_ERROR);
        }
    });

    let project_name = cli.name.or(cfg.name).unwrap_or_else(|| {
        project_dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unnamed_project".into())
    });

    let verbose = cli.verbose || cfg.verbose;

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
        eprintln!("Protocol error: expected HELLO_ACK, received {:?}", msg_type);
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

    // 5. Receive NEED frame
    let (msg_type, payload) = match read_frame(&mut stream).await {
        Ok(f) => f,
        Err(e) => {
            eprintln!("Error: failed to read NEED frame: {}", e);
            exit(EXIT_INFRA_ERROR);
        }
    };

    if msg_type != MsgType::Need {
        eprintln!("Protocol error: expected NEED frame, received {:?}", msg_type);
        exit(EXIT_INFRA_ERROR);
    }

    let need: NeedPayload = match decode_json(&payload) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("Error: invalid NEED payload: {}", e);
            exit(EXIT_INFRA_ERROR);
        }
    };

    // 6. Selective Delta Pack & Upload (FILES frame)
    let sync_start = Instant::now();
    if need.want.is_empty() {
        if verbose {
            println!("[Delta Sync] Remote workspace is completely up to date. 0 files to transfer!");
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
    let run = RunPayload {
        argv: cli.command,
        outputs,
        cwd: None,
        template,
        no_cache,
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
