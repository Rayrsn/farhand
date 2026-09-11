use protocol::{
    decode_json, read_frame, write_frame, write_json_frame, HelloAckPayload, HelloPayload,
    LogPayload, ManifestPayload, MsgType, NeedPayload, ResultPayload, RunPayload,
    CURRENT_PROTOCOL_VERSION,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWrite, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::Command;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

pub async fn run_server(
    listener: TcpListener,
    expected_token: Option<String>,
    workdir: PathBuf,
    custom_shell: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let token = Arc::new(expected_token);
    let workdir = Arc::new(workdir);
    let shell = Arc::new(custom_shell);

    loop {
        match listener.accept().await {
            Ok((stream, addr)) => {
                info!("Accepted connection from {}", addr);
                let token_clone = Arc::clone(&token);
                let workdir_clone = Arc::clone(&workdir);
                let shell_clone = Arc::clone(&shell);
                tokio::spawn(async move {
                    if let Err(e) =
                        handle_connection(stream, token_clone, workdir_clone, shell_clone).await
                    {
                        error!("Connection from {} error: {}", addr, e);
                    }
                    info!("Connection from {} closed", addr);
                });
            }
            Err(e) => {
                warn!("Accept failed: {}", e);
            }
        }
    }
}

pub async fn handle_connection(
    mut stream: TcpStream,
    expected_token: Arc<Option<String>>,
    workdir_root: Arc<PathBuf>,
    custom_shell: Arc<Option<String>>,
) -> Result<(), Box<dyn std::error::Error>> {
    // 1. Handshake: Expect MsgHello
    let (msg_type, payload) = read_frame(&mut stream).await?;
    if msg_type != MsgType::Hello {
        let ack = HelloAckPayload {
            ok: false,
            error: Some("Expected HELLO frame".into()),
        };
        write_json_frame(&mut stream, MsgType::HelloAck, &ack).await?;
        return Err("Protocol error: expected HELLO".into());
    }

    let hello: HelloPayload = decode_json(&payload)?;
    if hello.protocol_version != CURRENT_PROTOCOL_VERSION {
        let ack = HelloAckPayload {
            ok: false,
            error: Some(format!(
                "Protocol version mismatch: expected {}, got {}",
                CURRENT_PROTOCOL_VERSION, hello.protocol_version
            )),
        };
        write_json_frame(&mut stream, MsgType::HelloAck, &ack).await?;
        return Err("Protocol version mismatch".into());
    }

    if let Some(token) = expected_token.as_ref() {
        if &hello.token != token {
            let ack = HelloAckPayload {
                ok: false,
                error: Some("Unauthorized: invalid auth token".into()),
            };
            write_json_frame(&mut stream, MsgType::HelloAck, &ack).await?;
            return Err("Unauthorized".into());
        }
    }

    // Acknowledge handshake
    let ack = HelloAckPayload {
        ok: true,
        error: None,
    };
    write_json_frame(&mut stream, MsgType::HelloAck, &ack).await?;
    info!("Handshake successful for project '{}'", hello.project);

    // Resolve persistent workspace directory
    let workspace_dir = workspace::resolve_workspace_dir(&workdir_root, &hello.project);
    fs::create_dir_all(&workspace_dir)?;
    info!("Using persistent workspace: {}", workspace_dir.display());

    // 2. Receive MANIFEST frame
    let (msg_type, payload) = read_frame(&mut stream).await?;
    if msg_type != MsgType::Manifest {
        return Err(format!("Expected MANIFEST frame, got {:?}", msg_type).into());
    }
    let manifest: ManifestPayload = decode_json(&payload)?;
    info!(
        "Received client manifest with {} files. Diffing against workspace cache...",
        manifest.files.len()
    );

    let diff = workspace::diff_manifests(&workspace_dir, &manifest, &[])?;
    info!(
        "Diff computed: {} files needed, {} extraneous files flagged for deletion",
        diff.want.len(),
        diff.delete_extraneous.len()
    );

    // 3. Send NEED frame
    let need = NeedPayload {
        want: diff.want.clone(),
        delete_extraneous: diff.delete_extraneous.clone(),
    };
    write_json_frame(&mut stream, MsgType::Need, &need).await?;

    // 4. Receive FILES frame (delta tar.gz)
    let (msg_type, payload) = read_frame(&mut stream).await?;
    if msg_type != MsgType::Files {
        return Err(format!("Expected FILES frame, got {:?}", msg_type).into());
    }

    if !payload.is_empty() {
        info!("Unpacking {} delta bytes into workspace", payload.len());
        fileset::unpack_tar(&workspace_dir, &payload)?;
    } else {
        info!("Zero delta bytes uploaded (workspace up to date)");
    }

    // Apply deletions of extraneous files
    if !diff.delete_extraneous.is_empty() {
        let deleted = workspace::apply_deletions(&workspace_dir, &diff.delete_extraneous)?;
        info!("Deleted {} extraneous files from workspace", deleted);
    }

    // 5. Receive RUN frame
    let (msg_type, payload) = read_frame(&mut stream).await?;
    if msg_type != MsgType::Run {
        return Err(format!("Expected RUN frame, got {:?}", msg_type).into());
    }
    let run: RunPayload = decode_json(&payload)?;
    info!("Executing command: {:?}", run.argv);

    // 6. Execute command and stream output
    let (read_half, write_half) = stream.into_split();
    let shared_writer = Arc::new(Mutex::new(write_half));

    let exit_code = execute_and_stream(
        shared_writer.clone(),
        &workspace_dir,
        &run.argv,
        custom_shell.as_deref(),
    )
    .await?;

    info!("Command exited with status code {}", exit_code);

    // 7. Send RESULT frame
    let result = ResultPayload {
        exit_code,
        error: None,
    };
    {
        let mut writer = shared_writer.lock().await;
        write_json_frame(&mut *writer, MsgType::Result, &result).await?;
    }

    // 8. If command succeeded, resolve and send artifacts
    if exit_code == 0 {
        let artifact_paths =
            workspace::resolve_artifact_paths(&workspace_dir, run.outputs.as_deref());
        if !artifact_paths.is_empty() {
            info!("Packing {} artifact paths", artifact_paths.len());
            let tar_gz = fileset::pack_tar(&workspace_dir, &artifact_paths)?;
            let mut writer = shared_writer.lock().await;
            write_frame(&mut *writer, MsgType::Artifacts, &tar_gz).await?;
            info!("Sent ARTIFACTS frame ({} bytes)", tar_gz.len());
        }
    }

    drop(read_half);
    Ok(())
}

fn shell_escape(arg: &str) -> String {
    if arg.is_empty() {
        return "''".to_string();
    }
    if arg
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '=' | '@'))
    {
        return arg.to_string();
    }
    format!("'{}'", arg.replace('\'', "'\\''"))
}

pub async fn execute_and_stream<W: AsyncWrite + Unpin + Send + 'static>(
    writer: Arc<Mutex<W>>,
    cwd: &Path,
    argv: &[String],
    custom_shell: Option<&str>,
) -> Result<i32, Box<dyn std::error::Error>> {
    if argv.is_empty() {
        return Ok(0);
    }

    let joined_cmd = argv
        .iter()
        .map(|a| shell_escape(a))
        .collect::<Vec<_>>()
        .join(" ");

    let mut cmd = if let Some(shell_override) = custom_shell {
        let parts: Vec<&str> = shell_override.split_whitespace().collect();
        let mut c = Command::new(parts[0]);
        for part in &parts[1..] {
            c.arg(part);
        }
        c.arg(&joined_cmd);
        c
    } else if cfg!(windows) {
        let mut c = Command::new("cmd.exe");
        c.arg("/C").arg(&joined_cmd);
        c
    } else {
        let mut c = Command::new("/bin/sh");
        c.arg("-c").arg(&joined_cmd);
        c
    };

    cmd.current_dir(cwd);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn()?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let writer_out = Arc::clone(&writer);
    let stdout_handle = tokio::spawn(async move {
        if let Some(out) = stdout {
            let mut reader = BufReader::new(out).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let payload = LogPayload {
                    stream: "stdout".into(),
                    data: format!("{}\n", line),
                };
                let mut w = writer_out.lock().await;
                if write_json_frame(&mut *w, MsgType::Log, &payload).await.is_err() {
                    break;
                }
            }
        }
    });

    let writer_err = Arc::clone(&writer);
    let stderr_handle = tokio::spawn(async move {
        if let Some(err) = stderr {
            let mut reader = BufReader::new(err).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let payload = LogPayload {
                    stream: "stderr".into(),
                    data: format!("{}\n", line),
                };
                let mut w = writer_err.lock().await;
                if write_json_frame(&mut *w, MsgType::Log, &payload).await.is_err() {
                    break;
                }
            }
        }
    });

    let status = child.wait().await?;
    let _ = tokio::join!(stdout_handle, stderr_handle);

    Ok(status.code().unwrap_or(1))
}
