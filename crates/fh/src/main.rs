use clap::Parser;
use protocol::{
    decode_json, read_frame, write_frame, write_json_frame, FileEntry, HelloAckPayload,
    HelloPayload, LogPayload, ManifestPayload, MsgType, NeedPayload, ResultPayload, RunPayload,
    CURRENT_PROTOCOL_VERSION,
};
use std::io::Write;
use std::path::PathBuf;
use std::process::exit;
use tokio::net::TcpStream;

const EXIT_INFRA_ERROR: i32 = 125;

#[derive(Parser, Debug)]
#[command(name = "fh", about = "Farhand client: offload build/test execution to remote agent")]
struct Cli {
    #[arg(long, default_value = "127.0.0.1:9876", help = "Agent address, host:port")]
    host: String,

    #[arg(long, env = "FARHAND_TOKEN", help = "Shared authentication token")]
    token: Option<String>,

    #[arg(long, default_value = ".", help = "Local directory to sync")]
    dir: PathBuf,

    #[arg(long, help = "Project name / workspace key (default: local dir basename)")]
    name: Option<String>,

    #[arg(short, long, help = "Print detailed sync and timing statistics")]
    verbose: bool,

    #[arg(trailing_var_arg = true, required = true, help = "Command to run remotely")]
    command: Vec<String>,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let project_dir = match cli.dir.canonicalize() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Error: invalid project directory '{}': {}", cli.dir.display(), e);
            exit(EXIT_INFRA_ERROR);
        }
    };

    let project_name = cli.name.unwrap_or_else(|| {
        project_dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unnamed_project".into())
    });

    if cli.verbose {
        println!("=== Farhand Remote Runner ===");
        println!("Connecting to agent at: {}", cli.host);
        println!("Project: {} ({})", project_name, project_dir.display());
        println!("Remote command: {:?}", cli.command);
    }

    // 1. Connect TCP to agent
    let mut stream = match TcpStream::connect(&cli.host).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "Error: unable to reach agent at {} ({}).\nHint: ensure fhd is running and reachable.",
                cli.host, e
            );
            exit(EXIT_INFRA_ERROR);
        }
    };

    // 2. Handshake: Send HELLO
    let hello = HelloPayload {
        token: cli.token.unwrap_or_default(),
        project: project_name,
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
    let scan_start = std::time::Instant::now();
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

    if cli.verbose {
        println!(
            "Scanned {} local files in {:?}",
            manifest.files.len(),
            scan_start.elapsed()
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
    let sync_start = std::time::Instant::now();
    if need.want.is_empty() {
        if cli.verbose {
            println!("[Delta Sync] Remote workspace is completely up to date. 0 files to transfer!");
        }
        if let Err(e) = write_frame(&mut stream, MsgType::Files, &[]).await {
            eprintln!("Error: failed to send empty FILES frame: {}", e);
            exit(EXIT_INFRA_ERROR);
        }
    } else {
        if cli.verbose {
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

        if cli.verbose {
            println!(
                "[Delta Sync] Uploading {} bytes (compressed) across {} files in {:?}",
                tar_gz.len(),
                need.want.len(),
                sync_start.elapsed()
            );
        }

        if let Err(e) = write_frame(&mut stream, MsgType::Files, &tar_gz).await {
            eprintln!("Error: failed to send FILES frame: {}", e);
            exit(EXIT_INFRA_ERROR);
        }
    }

    // 7. Send RUN frame
    let run = RunPayload {
        argv: cli.command,
        outputs: None,
        cwd: None,
        template: None,
        no_cache: false,
    };

    if let Err(e) = write_json_frame(&mut stream, MsgType::Run, &run).await {
        eprintln!("Error: failed to send RUN frame: {}", e);
        exit(EXIT_INFRA_ERROR);
    }

    // 8. Receive streamed LOG and final RESULT frames
    let mut exit_code = 1;
    let mut stdout = std::io::stdout();
    let mut stderr = std::io::stderr();

    loop {
        let (msg_type, payload) = match read_frame(&mut stream).await {
            Ok(f) => f,
            Err(e) => {
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
                if let Ok(res) = decode_json::<ResultPayload>(&payload) {
                    exit_code = res.exit_code;
                    if let Some(err_msg) = res.error {
                        eprintln!("Remote error: {}", err_msg);
                    }
                }
                break;
            }
            other => {
                if cli.verbose {
                    println!("[farhand] Received control frame: {:?}", other);
                }
            }
        }
    }

    exit(exit_code);
}
