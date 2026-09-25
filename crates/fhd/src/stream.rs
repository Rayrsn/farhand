//! Output streaming: non-PTY child processes, PTY sessions, reverse port
//! forwarding, and the two `execute_*` entry points the connection handler
//! calls into.

use crate::exec::{
    apply_toolchain_env, apply_toolchain_pty, build_raw_shell_command, build_shell_command,
    kill_process_group, parse_custom_shell, resolve_shell_executable, shell_escape,
    wrap_command_with_toolchain,
};
use protocol::{
    read_frame, write_json_frame, LogPayload, MsgType, PortClosePayload, PortDataPayload,
    PortOpenPayload, ResizePayload,
};
use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::Mutex;
use tracing::warn;

pub async fn handle_port_open<W: AsyncWrite + Unpin + Send + 'static>(
    writer: Arc<Mutex<W>>,
    channels: Arc<Mutex<HashMap<u32, tokio::sync::mpsc::Sender<Vec<u8>>>>>,
    channel_id: u32,
    target_port: u16,
) {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(128);
    channels.lock().await.insert(channel_id, tx);
    let writer_clone = writer.clone();
    let channels_clone = channels.clone();

    tokio::spawn(async move {
        match tokio::net::TcpStream::connect(("127.0.0.1", target_port)).await {
            Ok(stream) => {
                let (mut tcp_read, mut tcp_write) = stream.into_split();
                let writer_in = writer_clone.clone();

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
                        let mut w = writer_in.lock().await;
                        if write_json_frame(&mut *w, MsgType::PortData, &payload)
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    let close = PortClosePayload { channel_id };
                    let mut w = writer_in.lock().await;
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
            }
            Err(e) => {
                warn!("Failed to connect to target port {}: {}", target_port, e);
                let close = PortClosePayload { channel_id };
                let mut w = writer_clone.lock().await;
                let _ = write_json_frame(&mut *w, MsgType::PortClose, &close).await;
            }
        }
        channels_clone.lock().await.remove(&channel_id);
    });
}

pub async fn run_child_and_stream<
    W: AsyncWrite + Unpin + Send + 'static,
    R: tokio::io::AsyncRead + Unpin + Send,
>(
    writer: Arc<Mutex<W>>,
    reader: &mut R,
    mut cmd: Command,
    raw_stdio: bool,
) -> Result<i32, Box<dyn std::error::Error>> {
    if raw_stdio {
        cmd.stdin(Stdio::piped());
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let err_msg = format!("farhand: failed to spawn command: {}\n", e);
            let payload = LogPayload {
                stream: "stderr".into(),
                data: err_msg,
            };
            let mut w = writer.lock().await;
            let _ = write_json_frame(&mut *w, MsgType::Log, &payload).await;
            return Ok(127);
        }
    };
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let mut child_stdin = child.stdin.take();

    let writer_out = Arc::clone(&writer);
    let stdout_handle = tokio::spawn(async move {
        if let Some(mut out) = stdout {
            if raw_stdio {
                use tokio::io::AsyncReadExt;
                let mut buf = [0u8; 8192];
                while let Ok(n) = out.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    let data = String::from_utf8_lossy(&buf[..n]).to_string();
                    let payload = LogPayload {
                        stream: "stdout".into(),
                        data,
                    };
                    let mut w = writer_out.lock().await;
                    if write_json_frame(&mut *w, MsgType::Log, &payload)
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            } else {
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
        }
    });

    let writer_err = Arc::clone(&writer);
    let stderr_handle = tokio::spawn(async move {
        if let Some(mut err) = stderr {
            if raw_stdio {
                use tokio::io::AsyncReadExt;
                let mut buf = [0u8; 8192];
                while let Ok(n) = err.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    let data = String::from_utf8_lossy(&buf[..n]).to_string();
                    let payload = LogPayload {
                        stream: "stderr".into(),
                        data,
                    };
                    let mut w = writer_err.lock().await;
                    if write_json_frame(&mut *w, MsgType::Log, &payload)
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            } else {
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
        }
    });

    let port_channels = Arc::new(Mutex::new(
        HashMap::<u32, tokio::sync::mpsc::Sender<Vec<u8>>>::new(),
    ));

    let exit_code = loop {
        tokio::select! {
            status_res = child.wait() => {
                let status = status_res?;
                let _ = tokio::join!(stdout_handle, stderr_handle);
                break status.code().unwrap_or(1);
            }
            frame_res = read_frame(reader) => {
                match frame_res {
                    Ok((MsgType::Stdin, payload)) => {
                        if let Some(cin) = &mut child_stdin {
                            use tokio::io::AsyncWriteExt;
                            let _ = cin.write_all(&payload).await;
                            let _ = cin.flush().await;
                        }
                    }
                    Ok((MsgType::PortOpen, payload)) => {
                        if let Ok(po) = serde_json::from_slice::<PortOpenPayload>(&payload) {
                            handle_port_open(writer.clone(), port_channels.clone(), po.channel_id, po.target_port).await;
                        }
                    }
                    Ok((MsgType::PortData, payload)) => {
                        if let Ok(pd) = serde_json::from_slice::<PortDataPayload>(&payload) {
                            let map = port_channels.lock().await;
                            if let Some(tx) = map.get(&pd.channel_id) {
                                let _ = tx.send(pd.data).await;
                            }
                        }
                    }
                    Ok((MsgType::PortClose, payload)) => {
                        if let Ok(pc) = serde_json::from_slice::<PortClosePayload>(&payload) {
                            port_channels.lock().await.remove(&pc.channel_id);
                        }
                    }
                    Ok(_) => {}
                    Err(_) => {
                        warn!("Client disconnected while command was executing. Terminating process group.");
                        kill_process_group(&mut child).await;
                        return Err("Client disconnected".into());
                    }
                }
            }
        }
    };

    Ok(exit_code)
}

#[allow(clippy::too_many_arguments)]
#[allow(unsafe_code)] // FFI: kill(2) on the PTY child's own process group on disconnect.
pub async fn run_pty_child_and_stream<
    W: AsyncWrite + Unpin + Send + 'static,
    R: tokio::io::AsyncRead + Unpin + Send,
>(
    writer: Arc<Mutex<W>>,
    reader: &mut R,
    cwd: &Path,
    argv: &[String],
    custom_shell: Option<&str>,
    env: Option<&std::collections::HashMap<String, String>>,
    toolchain: Option<&std::collections::HashMap<String, String>>,
    cols: Option<u16>,
    rows: Option<u16>,
) -> Result<i32, Box<dyn std::error::Error>> {
    let pty_system = portable_pty::native_pty_system();
    let initial_size = portable_pty::PtySize {
        rows: rows.unwrap_or(24),
        cols: cols.unwrap_or(80),
        pixel_width: 0,
        pixel_height: 0,
    };
    let pair = pty_system.openpty(initial_size)?;

    let is_shell_request = argv.is_empty() || argv[0] == "$SHELL" || argv[0] == "shell";

    let mut cmd_builder = if is_shell_request {
        let sh_bin = resolve_shell_executable("$SHELL");
        let mut cb = portable_pty::CommandBuilder::new(&sh_bin);
        if !cfg!(windows) {
            cb.arg("-l");
        }
        cb
    } else if let Some(shell_override) = custom_shell {
        let shell_tokens = parse_custom_shell(shell_override)
            .unwrap_or_else(|| vec!["/bin/sh".to_string(), "-l".to_string()]);
        let mut cb = portable_pty::CommandBuilder::new(&shell_tokens[0]);
        for part in &shell_tokens[1..] {
            cb.arg(part);
        }
        let joined_cmd = argv
            .iter()
            .map(|a| shell_escape(a))
            .collect::<Vec<_>>()
            .join(" ");
        let wrapped_cmd = wrap_command_with_toolchain(&joined_cmd, toolchain);
        cb.arg(&wrapped_cmd);
        cb
    } else if !argv.is_empty()
        && (argv[0].contains('/')
            || argv[0].contains('\\')
            || (cfg!(windows)
                && (argv[0].eq_ignore_ascii_case("cmd.exe")
                    || argv[0].eq_ignore_ascii_case("cmd"))))
    {
        let mut cb = portable_pty::CommandBuilder::new(&argv[0]);
        for arg in &argv[1..] {
            cb.arg(arg);
        }
        cb
    } else if cfg!(windows) {
        let has_shell_metachars = argv.iter().any(|arg| {
            arg.contains('&')
                || arg.contains('|')
                || arg.contains(';')
                || arg.contains('>')
                || arg.contains('<')
                || arg.contains('^')
                || arg.contains('%')
                || arg.contains('`')
        });

        if !has_shell_metachars && !argv.is_empty() {
            let mut cb = portable_pty::CommandBuilder::new(&argv[0]);
            for arg in &argv[1..] {
                cb.arg(arg);
            }
            cb
        } else {
            let joined_cmd = argv
                .iter()
                .map(|a| shell_escape(a))
                .collect::<Vec<_>>()
                .join(" ");
            let wrapped_cmd = wrap_command_with_toolchain(&joined_cmd, toolchain);
            let mut cb = portable_pty::CommandBuilder::new("cmd.exe");
            cb.arg("/C");
            cb.arg(&wrapped_cmd);
            cb
        }
    } else {
        let has_shell_metachars = argv.iter().any(|arg| {
            arg.contains('&')
                || arg.contains('|')
                || arg.contains(';')
                || arg.contains('>')
                || arg.contains('<')
                || arg.contains('$')
                || arg.contains('`')
        });

        if !has_shell_metachars && !argv.is_empty() {
            let resolved_bin = resolve_shell_executable(&argv[0]);
            let mut cb = portable_pty::CommandBuilder::new(&resolved_bin);
            for arg in &argv[1..] {
                cb.arg(arg);
            }
            cb
        } else {
            let joined_cmd = argv
                .iter()
                .map(|a| shell_escape(a))
                .collect::<Vec<_>>()
                .join(" ");
            let wrapped_cmd = wrap_command_with_toolchain(&joined_cmd, toolchain);
            let mut cb = portable_pty::CommandBuilder::new("/bin/sh");
            cb.arg("-c");
            cb.arg(&wrapped_cmd);
            cb
        }
    };

    cmd_builder.cwd(cwd);
    if let Some(envs) = env {
        for (k, v) in envs {
            cmd_builder.env(k, v);
        }
    }
    apply_toolchain_pty(&mut cmd_builder, toolchain);
    if env.map(|e| !e.contains_key("TERM")).unwrap_or(true) {
        cmd_builder.env("TERM", "xterm-256color");
    }

    let mut child = match pair.slave.spawn_command(cmd_builder) {
        Ok(c) => c,
        Err(e) => {
            let err_msg = format!("farhand: failed to spawn '{:?}': {}\r\n", argv, e);
            let payload = LogPayload {
                stream: "stderr".into(),
                data: err_msg,
            };
            let mut w = writer.lock().await;
            let _ = write_json_frame(&mut *w, MsgType::Log, &payload).await;
            return Ok(127);
        }
    };
    drop(pair.slave);

    let _child_pid = child.process_id();
    let mut pty_reader = pair.master.try_clone_reader()?;
    let pty_writer = Arc::new(Mutex::new(pair.master.take_writer()?));
    let master = Arc::new(Mutex::new(pair.master));

    let (log_tx, mut log_rx) = tokio::sync::mpsc::channel::<LogPayload>(128);
    let writer_out = Arc::clone(&writer);
    let log_writer_task = tokio::spawn(async move {
        while let Some(payload) = log_rx.recv().await {
            let mut w = writer_out.lock().await;
            if write_json_frame(&mut *w, MsgType::Log, &payload)
                .await
                .is_err()
            {
                break;
            }
        }
    });

    let output_handle = tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut buf = [0u8; 4096];
        while let Ok(n) = pty_reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            let text = String::from_utf8_lossy(&buf[..n]).to_string();
            let payload = LogPayload {
                stream: "stdout".into(),
                data: text,
            };
            if log_tx.blocking_send(payload).is_err() {
                break;
            }
        }
    });

    let port_channels = Arc::new(Mutex::new(
        HashMap::<u32, tokio::sync::mpsc::Sender<Vec<u8>>>::new(),
    ));

    let (exit_tx, mut exit_rx) = tokio::sync::oneshot::channel();
    // Blocking child wait off the runtime thread — no 20 ms polling latency,
    // no executor stalls. The oneshot delivers the exit code to the select!.
    tokio::task::spawn_blocking(move || {
        let code = match child.wait() {
            Ok(status) => {
                if status.success() {
                    0
                } else {
                    status.exit_code() as i32
                }
            }
            Err(_) => 1,
        };
        let _ = exit_tx.send(code);
    });

    let exit_code = loop {
        tokio::select! {
            code = &mut exit_rx => {
                break code.unwrap_or(1);
            }
            frame_res = read_frame(reader) => {
                match frame_res {
                    Ok((MsgType::Stdin, payload)) => {
                        let mut pw = pty_writer.lock().await;
                        use std::io::Write;
                        let _ = pw.write_all(&payload);
                        let _ = pw.flush();
                    }
                    Ok((MsgType::Resize, payload)) => {
                        if let Ok(resize) = serde_json::from_slice::<ResizePayload>(&payload) {
                            let m = master.lock().await;
                            let _ = m.resize(portable_pty::PtySize {
                                rows: resize.rows,
                                cols: resize.cols,
                                pixel_width: 0,
                                pixel_height: 0,
                            });
                        }
                    }
                    Ok((MsgType::PortOpen, payload)) => {
                        if let Ok(po) = serde_json::from_slice::<PortOpenPayload>(&payload) {
                            handle_port_open(writer.clone(), port_channels.clone(), po.channel_id, po.target_port).await;
                        }
                    }
                    Ok((MsgType::PortData, payload)) => {
                        if let Ok(pd) = serde_json::from_slice::<PortDataPayload>(&payload) {
                            let map = port_channels.lock().await;
                            if let Some(tx) = map.get(&pd.channel_id) {
                                let _ = tx.send(pd.data).await;
                            }
                        }
                    }
                    Ok((MsgType::PortClose, payload)) => {
                        if let Ok(pc) = serde_json::from_slice::<PortClosePayload>(&payload) {
                            port_channels.lock().await.remove(&pc.channel_id);
                        }
                    }
                    Ok(_) => {}
                    Err(_) => {
                        warn!("Client disconnected during PTY session. Terminating child.");
                        #[cfg(unix)]
                        if let Some(pid) = _child_pid {
                            // SAFETY: pid is the PTY child we spawned with
                            // `process_group(0)`, so `-pid` signals only that
                            // child's process group (AGENTS.md §3.4).
                            unsafe {
                                libc::kill(-(pid as i32), libc::SIGTERM);
                            }
                        }
                        #[cfg(windows)]
                        if let Some(pid) = _child_pid {
                            let _ = std::process::Command::new("taskkill")
                                .args(["/F", "/T", "/PID", &pid.to_string()])
                                .output();
                        }
                        return Err("Client disconnected".into());
                    }
                }
            }
        }
    };

    drop(master);
    drop(pty_writer);
    let _ = tokio::time::timeout(tokio::time::Duration::from_millis(300), output_handle).await;
    let _ = tokio::time::timeout(tokio::time::Duration::from_millis(300), log_writer_task).await;
    Ok(exit_code)
}

pub async fn execute_raw_command_and_stream<
    W: AsyncWrite + Unpin + Send + 'static,
    R: tokio::io::AsyncRead + Unpin + Send,
>(
    writer: Arc<Mutex<W>>,
    reader: &mut R,
    cwd: &Path,
    raw_cmd: &str,
    custom_shell: Option<&str>,
    env: Option<&std::collections::HashMap<String, String>>,
    toolchain: Option<&std::collections::HashMap<String, String>>,
) -> Result<i32, Box<dyn std::error::Error>> {
    let mut cmd = build_raw_shell_command(cwd, raw_cmd, custom_shell, toolchain);
    if let Some(envs) = env {
        cmd.envs(envs);
    }
    apply_toolchain_env(&mut cmd, toolchain);
    run_child_and_stream(writer, reader, cmd, false).await
}

#[allow(clippy::too_many_arguments)]
pub async fn execute_and_stream<
    W: AsyncWrite + Unpin + Send + 'static,
    R: tokio::io::AsyncRead + Unpin + Send,
>(
    writer: Arc<Mutex<W>>,
    reader: &mut R,
    cwd: &Path,
    argv: &[String],
    custom_shell: Option<&str>,
    env: Option<&std::collections::HashMap<String, String>>,
    toolchain: Option<&std::collections::HashMap<String, String>>,
    tty: bool,
    cols: Option<u16>,
    rows: Option<u16>,
    raw_stdio: bool,
) -> Result<i32, Box<dyn std::error::Error>> {
    if argv.is_empty() {
        return Ok(0);
    }
    if tty {
        run_pty_child_and_stream(
            writer,
            reader,
            cwd,
            argv,
            custom_shell,
            env,
            toolchain,
            cols,
            rows,
        )
        .await
    } else {
        let mut cmd = build_shell_command(cwd, argv, custom_shell, toolchain);
        if let Some(envs) = env {
            cmd.envs(envs);
        }
        apply_toolchain_env(&mut cmd, toolchain);
        run_child_and_stream(writer, reader, cmd, raw_stdio).await
    }
}
