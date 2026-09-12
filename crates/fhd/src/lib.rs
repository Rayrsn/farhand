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

pub fn get_hostname() -> String {
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        let res = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
        if res == 0 {
            let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            if let Ok(s) = std::str::from_utf8(&buf[..len]) {
                if !s.is_empty() {
                    return s.to_string();
                }
            }
        }
    }
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "fhd-agent".to_string())
}

#[derive(Clone)]
pub struct ServerContext {
    pub expected_token: Option<String>,
    pub workdir_root: PathBuf,
    pub custom_shell: Option<String>,
    pub semaphore: Arc<tokio::sync::Semaphore>,
    pub lock_manager: workspace::WorkspaceLockManager,
    pub tags: Vec<String>,
    pub queue_depth: Arc<std::sync::atomic::AtomicUsize>,
    pub max_runs: usize,
}

pub async fn run_server(
    listener: TcpListener,
    expected_token: Option<String>,
    workdir: PathBuf,
    custom_shell: Option<String>,
    max_concurrent_runs: Option<usize>,
    tags: Vec<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let max_runs = max_concurrent_runs.unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
    });
    let semaphore = Arc::new(tokio::sync::Semaphore::new(max_runs));
    let lock_manager = workspace::WorkspaceLockManager::new();
    let queue_depth = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let ctx = Arc::new(ServerContext {
        expected_token,
        workdir_root: workdir,
        custom_shell,
        semaphore,
        lock_manager,
        tags,
        queue_depth,
        max_runs,
    });

    loop {
        match listener.accept().await {
            Ok((stream, addr)) => {
                info!("Accepted connection from {}", addr);
                let ctx_clone = Arc::clone(&ctx);
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, ctx_clone).await {
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
    ctx: Arc<ServerContext>,
) -> Result<(), Box<dyn std::error::Error>> {
    // 1. First frame: can be STATUS probe or HELLO handshake
    let (msg_type, payload) = read_frame(&mut stream).await?;

    if msg_type == MsgType::Status {
        let status_req: protocol::StatusRequestPayload = decode_json(&payload)?;
        if let Some(expected) = ctx.expected_token.as_deref() {
            if !expected.is_empty() && status_req.token != expected {
                let ack = HelloAckPayload {
                    ok: false,
                    error: Some("Unauthorized STATUS request".into()),
                };
                write_json_frame(&mut stream, MsgType::HelloAck, &ack).await?;
                return Err("Unauthorized STATUS request".into());
            }
        }
        let active_runs = ctx
            .max_runs
            .saturating_sub(ctx.semaphore.available_permits());
        let depth = ctx.queue_depth.load(std::sync::atomic::Ordering::Relaxed);
        let resp = protocol::StatusResponsePayload {
            active_runs,
            max_runs: ctx.max_runs,
            queue_depth: depth,
            hostname: get_hostname(),
            tags: ctx.tags.clone(),
        };
        write_json_frame(&mut stream, MsgType::StatusResp, &resp).await?;
        return Ok(());
    }

    if msg_type != MsgType::Hello {
        let ack = HelloAckPayload {
            ok: false,
            error: Some("Expected HELLO or STATUS frame".into()),
        };
        write_json_frame(&mut stream, MsgType::HelloAck, &ack).await?;
        return Err("Protocol error: expected HELLO or STATUS".into());
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

    if let Some(token) = ctx.expected_token.as_deref() {
        if hello.token != token {
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
    let workspace_dir = workspace::resolve_workspace_dir(&ctx.workdir_root, &hello.project);
    fs::create_dir_all(&workspace_dir)?;
    info!("Using persistent workspace: {}", workspace_dir.display());

    // 2. Receive next frame: PUT_TEMPLATE or MANIFEST
    let (msg_type, payload) = read_frame(&mut stream).await?;
    let manifest: ManifestPayload = if msg_type == MsgType::PutTemplate {
        let put_req: protocol::PutTemplatePayload = decode_json(&payload)?;
        info!(
            "Received PUT_TEMPLATE for '{}' (scope: {})",
            put_req.name, put_req.scope
        );
        match templates::save_template(
            Some(&workspace_dir),
            &put_req.name,
            &put_req.yaml,
            &put_req.scope,
        ) {
            Ok(saved_path) => {
                info!("Saved template to {}", saved_path.display());
                let ack = HelloAckPayload {
                    ok: true,
                    error: None,
                };
                write_json_frame(&mut stream, MsgType::HelloAck, &ack).await?;
                return Ok(());
            }
            Err(e) => {
                warn!("Failed to save template: {}", e);
                let ack = HelloAckPayload {
                    ok: false,
                    error: Some(e.to_string()),
                };
                write_json_frame(&mut stream, MsgType::HelloAck, &ack).await?;
                return Err(format!("Failed to save template: {}", e).into());
            }
        }
    } else if msg_type == MsgType::Manifest {
        decode_json(&payload)?
    } else {
        return Err(format!(
            "Expected MANIFEST or PUT_TEMPLATE frame, got {:?}",
            msg_type
        )
        .into());
    };

    // Acquire per-project lock to ensure serialized execution on the same project workspace
    let project_mutex = ctx.lock_manager.get_lock(&hello.project).await;
    let _project_guard = match project_mutex.clone().try_lock_owned() {
        Ok(guard) => guard,
        Err(_) => {
            info!(
                "Project '{}' is busy. Sending QUEUED frame...",
                hello.project
            );
            ctx.queue_depth
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let pos = ctx.queue_depth.load(std::sync::atomic::Ordering::Relaxed);
            let queued = protocol::QueuedPayload {
                position: pos,
                reason: "project_busy".to_string(),
            };
            write_json_frame(&mut stream, MsgType::Queued, &queued).await?;
            let guard = project_mutex.lock_owned().await;
            ctx.queue_depth
                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            guard
        }
    };

    // Acquire global concurrency permit
    let _permit = match ctx.semaphore.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            info!("Agent concurrency limit reached. Sending QUEUED frame...");
            ctx.queue_depth
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let pos = ctx.queue_depth.load(std::sync::atomic::Ordering::Relaxed);
            let queued = protocol::QueuedPayload {
                position: pos,
                reason: "concurrency_limit".to_string(),
            };
            write_json_frame(&mut stream, MsgType::Queued, &queued).await?;
            let permit_res = ctx.semaphore.clone().acquire_owned().await;
            ctx.queue_depth
                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            permit_res.map_err(|e| e.to_string())?
        }
    };

    info!(
        "Received client manifest with {} files. Diffing against workspace cache...",
        manifest.files.len()
    );

    let extra_ignores = templates::resolve_template_extra_ignores(&workspace_dir, None);
    let diff = workspace::diff_manifests(&workspace_dir, &manifest, &extra_ignores)?;
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
    let (mut read_half, write_half) = stream.into_split();
    let shared_writer = Arc::new(Mutex::new(write_half));

    let exit_code = execute_and_stream(
        shared_writer.clone(),
        &mut read_half,
        &workspace_dir,
        &run.argv,
        ctx.custom_shell.as_deref(),
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
        let artifact_paths = workspace::resolve_artifact_paths(
            &workspace_dir,
            run.outputs.as_deref(),
            run.template.as_deref(),
        );
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

pub fn build_shell_command(cwd: &Path, argv: &[String], custom_shell: Option<&str>) -> Command {
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

    #[cfg(unix)]
    cmd.process_group(0);

    cmd
}

pub async fn kill_process_group(child: &mut tokio::process::Child) {
    if let Some(pid) = child.id() {
        #[cfg(unix)]
        {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGTERM);
            }
            tokio::select! {
                _ = child.wait() => {}
                _ = tokio::time::sleep(std::time::Duration::from_secs(3)) => {
                    unsafe {
                        libc::kill(-(pid as i32), libc::SIGKILL);
                    }
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = child.kill().await;
        }
    }
}

pub async fn execute_and_stream<
    W: AsyncWrite + Unpin + Send + 'static,
    R: tokio::io::AsyncRead + Unpin + Send,
>(
    writer: Arc<Mutex<W>>,
    reader: &mut R,
    cwd: &Path,
    argv: &[String],
    custom_shell: Option<&str>,
) -> Result<i32, Box<dyn std::error::Error>> {
    if argv.is_empty() {
        return Ok(0);
    }

    let mut cmd = build_shell_command(cwd, argv, custom_shell);
    let mut child = cmd.spawn()?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let writer_out = Arc::clone(&writer);
    let stdout_handle = tokio::spawn(async move {
        if let Some(out) = stdout {
            let mut reader = BufReader::new(out).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let clean_line = line.trim_end_matches('\r');
                let payload = LogPayload {
                    stream: "stdout".into(),
                    data: format!("{}\n", clean_line),
                };
                let mut w = writer_out.lock().await;
                if write_json_frame(&mut *w, MsgType::Log, &payload)
                    .await
                    .is_err()
                {
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
                let clean_line = line.trim_end_matches('\r');
                let payload = LogPayload {
                    stream: "stderr".into(),
                    data: format!("{}\n", clean_line),
                };
                let mut w = writer_err.lock().await;
                if write_json_frame(&mut *w, MsgType::Log, &payload)
                    .await
                    .is_err()
                {
                    break;
                }
            }
        }
    });

    let disconnect_monitor = async {
        use tokio::io::AsyncReadExt;
        let mut buf = [0u8; 1];
        match reader.read(&mut buf).await {
            Ok(0) => true,  // EOF: client disconnected
            Err(_) => true, // Connection reset/error
            Ok(_) => false,
        }
    };

    tokio::select! {
        status_res = child.wait() => {
            let status = status_res?;
            let _ = tokio::join!(stdout_handle, stderr_handle);
            Ok(status.code().unwrap_or(1))
        }
        _ = disconnect_monitor => {
            warn!("Client disconnected while command was executing. Terminating process group.");
            kill_process_group(&mut child).await;
            Err("Client disconnected".into())
        }
    }
}
