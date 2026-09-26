// Unsafe is allowed only at the FFI boundaries listed in CONTRIBUTING.md
// (gethostname, kill(2), platform resource probes), each with a SAFETY
// contract. Connection handling, sync, execution, and session logic must
// stay pure safe Rust.
#![deny(unsafe_code)]

pub mod metrics;
pub mod metrics_server;

// Module layout: `lib.rs` owns the server lifecycle (accept loop, TLS
// dispatch, per-connection run pipeline); everything else lives in a focused
// module. See each module's docs for its responsibility.
mod active;
mod exec;
mod session;
mod stream;

pub use exec::parse_custom_shell;
pub use session::{get_hostname, is_exposed_bind, validate_start_config, ServerContext};

use active::{next_run_id, ActiveBuildGuard};
use session::{deny_control_request, DEFAULT_MAX_CONNECTIONS, UNLIMITED_CONNECTIONS};
use stream::{execute_and_stream, execute_raw_command_and_stream};

use protocol::{
    decode_json, read_frame, read_frame_limited, write_frame, write_json_frame, FrameError,
    HelloAckPayload, HelloPayload, ManifestPayload, MsgType, NeedPayload, ResultPayload,
    RunPayload, CURRENT_PROTOCOL_VERSION,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

/// Accept loop and per-connection dispatch for the agent daemon.
///
/// The parameter list is long but each item is a distinct daemon policy
/// (limits, tags, storage, TLS, queueing); grouping them behind a config
/// struct would only move the same fields one call frame away. See
/// `ServerContext` for the subset the connection handler needs.
#[allow(clippy::too_many_arguments)]
pub async fn run_server(
    listener: TcpListener,
    expected_token: Option<String>,
    workdir: PathBuf,
    custom_shell: Option<String>,
    max_concurrent_runs: Option<usize>,
    tags: Vec<String>,
    min_disk_bytes: Option<u64>,
    cas_dir: Option<PathBuf>,
    no_cas: bool,
    tls_acceptor: Option<protocol::TlsAcceptor>,
    max_connections: Option<usize>,
    max_queued_runs: Option<usize>,
    lock_manager: Option<workspace::WorkspaceLockManager>,
    // When set, serve Prometheus metrics on this port, alongside the agent
    // protocol on the main listener.
    metrics_port: Option<u16>,
) -> Result<(), Box<dyn std::error::Error>> {
    let max_runs = max_concurrent_runs.unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
    });
    let semaphore = Arc::new(tokio::sync::Semaphore::new(max_runs));
    let lock_manager = lock_manager.unwrap_or_default();
    let queue_depth = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let connection_limiter = Arc::new(tokio::sync::Semaphore::new(match max_connections {
        Some(0) => UNLIMITED_CONNECTIONS,
        Some(n) => n,
        None => DEFAULT_MAX_CONNECTIONS,
    }));

    let cas_store = if !no_cas {
        let base = cas_dir.unwrap_or_else(|| workdir.clone());
        Some(workspace::CasStore::new(&base))
    } else {
        None
    };

    let ctx = Arc::new(ServerContext {
        expected_token,
        workdir_root: workdir,
        custom_shell,
        semaphore,
        lock_manager,
        tags,
        queue_depth,
        max_runs,
        min_disk_bytes: min_disk_bytes.unwrap_or(2_500_000_000), // Default: 2.5 GB
        cas_store,
        start_time: std::time::Instant::now(),
        active_builds: Arc::new(std::sync::Mutex::new(HashMap::new())),
        connection_limiter,
        max_queued_runs: max_queued_runs.unwrap_or(16),
    });

    if let Some(port) = metrics_port {
        match tokio::net::TcpListener::bind(("0.0.0.0", port)).await {
            Ok(metrics_listener) => {
                let metrics_ctx = Arc::clone(&ctx);
                tokio::spawn(async move {
                    metrics_server::serve_metrics(metrics_listener, metrics_ctx).await;
                });
            }
            Err(e) => {
                // A metrics port that cannot bind must not take the agent down
                // with it: the agent is what people depend on.
                error!("failed to bind metrics port {port} ({e}); metrics disabled");
            }
        }
    }

    loop {
        match listener.accept().await {
            Ok((stream, addr)) => {
                // Bound concurrent connections: excess sockets are closed
                // immediately so a flood cannot spawn unbounded tasks.
                let permit = match ctx.connection_limiter.clone().try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => {
                        warn!(
                            "Connection limit reached; dropping connection from {}",
                            addr
                        );
                        continue;
                    }
                };
                info!("Accepted connection from {}", addr);
                let ctx_clone = Arc::clone(&ctx);
                let tls_acceptor_clone = tls_acceptor.clone();
                tokio::spawn(async move {
                    let _permit = permit; // released when the connection task ends
                    let res = match tls_acceptor_clone {
                        Some(acceptor) => match acceptor.accept(stream).await {
                            Ok(tls_stream) => {
                                handle_connection(
                                    protocol::MaybeTlsStream::Server(tls_stream),
                                    ctx_clone,
                                    addr.to_string(),
                                )
                                .await
                            }
                            Err(e) => {
                                error!("TLS handshake failed with {}: {}", addr, e);
                                Err(e.into())
                            }
                        },
                        None => {
                            handle_connection(
                                protocol::MaybeTlsStream::Plain(stream),
                                ctx_clone,
                                addr.to_string(),
                            )
                            .await
                        }
                    };
                    if let Err(e) = res {
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

pub async fn handle_connection<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    mut stream: S,
    ctx: Arc<ServerContext>,
    client_addr: String,
) -> Result<(), Box<dyn std::error::Error>> {
    // 1. First frame: can be STATUS probe, HISTORY query, or HELLO handshake.
    //    Pre-authentication, so a strict size cap applies (see MAX_PRE_AUTH_PAYLOAD).
    let (msg_type, payload) =
        read_frame_limited(&mut stream, protocol::MAX_PRE_AUTH_PAYLOAD).await?;

    if msg_type == MsgType::Status {
        let status_req: protocol::StatusRequestPayload = decode_json(&payload)?;
        if !ctx.authorize(Some(status_req.token.as_str())) {
            return deny_control_request(&mut stream, "STATUS").await;
        }
        let active_runs = ctx
            .max_runs
            .saturating_sub(ctx.semaphore.available_permits());
        let depth = ctx.queue_depth.load(std::sync::atomic::Ordering::Relaxed);
        let (disk_free_bytes, disk_total_bytes) = match workspace::get_disk_space(&ctx.workdir_root)
        {
            Ok(space) => (Some(space.available_bytes), Some(space.total_bytes)),
            Err(_) => (None, None),
        };
        let (memory_used_bytes, memory_total_bytes) = metrics::get_memory_info();
        let cpu_count = Some(metrics::get_cpu_count());
        let load_averages = metrics::get_load_averages();
        let uptime_secs = Some(ctx.start_time.elapsed().as_secs());
        let workspaces_count = metrics::get_workspaces_count(&ctx.workdir_root);

        let active_builds: Vec<protocol::ActiveBuildInfo> = {
            let map = ctx
                .active_builds
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            map.iter()
                .map(|(id, (project, argv, start_instant, client_addr))| {
                    protocol::ActiveBuildInfo {
                        id: id.clone(),
                        project: project.clone(),
                        argv: argv.clone(),
                        elapsed_ms: start_instant.elapsed().as_millis() as u64,
                        client_addr: client_addr.clone(),
                    }
                })
                .collect()
        };
        // Guard is gone here — never held across an await.

        let resp = protocol::StatusResponsePayload {
            active_runs,
            max_runs: ctx.max_runs,
            queue_depth: depth,
            hostname: get_hostname(),
            tags: ctx.tags.clone(),
            disk_free_bytes,
            disk_total_bytes,
            cpu_count,
            load_averages,
            memory_used_bytes,
            memory_total_bytes,
            uptime_secs,
            active_builds: Some(active_builds),
            workspaces_count,
        };
        write_json_frame(&mut stream, MsgType::StatusResp, &resp).await?;
        return Ok(());
    }

    if msg_type == MsgType::History {
        let history_req: protocol::HistoryRequestPayload = decode_json(&payload)?;
        if !ctx.authorize(Some(history_req.token.as_str())) {
            return deny_control_request(&mut stream, "HISTORY").await;
        }
        let workspace_dir =
            workspace::resolve_workspace_dir(&ctx.workdir_root, &history_req.project);
        let runs = workspace::history::get_recent_runs(&workspace_dir, history_req.limit)
            .unwrap_or_default();
        let resp = protocol::HistoryResponsePayload {
            project: history_req.project,
            runs,
        };
        write_json_frame(&mut stream, MsgType::HistoryResp, &resp).await?;
        return Ok(());
    }

    if msg_type == MsgType::Clean {
        let clean_req: protocol::CleanRequestPayload = decode_json(&payload)?;
        if !ctx.authorize(Some(clean_req.token.as_str())) {
            return deny_control_request(&mut stream, "CLEAN").await;
        }

        let mut bytes_freed = 0u64;
        let msg;

        if clean_req.all_branches {
            let base_name = workspace::parse_base_project_name(&clean_req.project)
                .unwrap_or(&clean_req.project);
            let workspaces = workspace::scan_workspaces(&ctx.workdir_root);
            let mut count = 0;
            let mut skipped_busy = 0;
            for ws in workspaces {
                if !ws.is_canonical && ws.name.starts_with(base_name) {
                    // Never delete a workspace whose project is mid-run.
                    if ctx.lock_manager.is_locked(&ws.name).await {
                        skipped_busy += 1;
                        continue;
                    }
                    bytes_freed += ws.size_bytes;
                    if let Err(e) = std::fs::remove_dir_all(&ws.path) {
                        warn!("CLEAN: failed to remove {}: {}", ws.path.display(), e);
                    } else {
                        count += 1;
                    }
                }
            }
            msg = format!(
                "Purged {} branch workspaces for project '{}'{}",
                count,
                base_name,
                if skipped_busy > 0 {
                    format!(" ({} skipped: active run)", skipped_busy)
                } else {
                    String::new()
                }
            );
        } else {
            let ws_dir = workspace::resolve_workspace_dir(&ctx.workdir_root, &clean_req.project);
            if ws_dir.is_dir() {
                if ctx.lock_manager.is_locked(&clean_req.project).await {
                    msg = format!(
                        "Workspace '{}' is busy (active run); try again after it finishes",
                        clean_req.project
                    );
                    let resp = protocol::CleanResponsePayload {
                        ok: false,
                        message: msg,
                        bytes_freed: 0,
                    };
                    write_json_frame(&mut stream, MsgType::CleanResp, &resp).await?;
                    return Ok(());
                }
                if clean_req.caches_only {
                    bytes_freed = workspace::trim_workspace_caches(&ws_dir);
                    msg = format!(
                        "Trimmed volatile caches for workspace '{}'",
                        clean_req.project
                    );
                } else {
                    bytes_freed = workspace::calculate_dir_size(&ws_dir);
                    if let Err(e) = std::fs::remove_dir_all(&ws_dir) {
                        warn!("CLEAN: failed to remove {}: {}", ws_dir.display(), e);
                        let resp = protocol::CleanResponsePayload {
                            ok: false,
                            message: format!(
                                "Failed to remove workspace '{}': {}",
                                clean_req.project, e
                            ),
                            bytes_freed: 0,
                        };
                        write_json_frame(&mut stream, MsgType::CleanResp, &resp).await?;
                        return Ok(());
                    }
                    msg = format!("Removed workspace '{}'", clean_req.project);
                }
            } else {
                msg = format!("Workspace '{}' does not exist remotely", clean_req.project);
            }
        }

        let resp = protocol::CleanResponsePayload {
            ok: true,
            message: msg,
            bytes_freed,
        };
        write_json_frame(&mut stream, MsgType::CleanResp, &resp).await?;
        return Ok(());
    }

    if msg_type != MsgType::Hello {
        let ack = HelloAckPayload {
            ok: false,
            error: Some("Expected HELLO, STATUS, HISTORY, or CLEAN frame".into()),
            compression: None,
            remote_workdir: None,
        };
        write_json_frame(&mut stream, MsgType::HelloAck, &ack).await?;
        return Err("Protocol error: expected HELLO, STATUS, HISTORY, or CLEAN".into());
    }

    let hello: HelloPayload = decode_json(&payload)?;
    if hello.protocol_version != CURRENT_PROTOCOL_VERSION {
        let ack = HelloAckPayload {
            ok: false,
            error: Some(format!(
                "Protocol version mismatch: expected {}, got {}",
                CURRENT_PROTOCOL_VERSION, hello.protocol_version
            )),
            compression: None,
            remote_workdir: None,
        };
        write_json_frame(&mut stream, MsgType::HelloAck, &ack).await?;
        return Err("Protocol version mismatch".into());
    }

    if !ctx.authorize(Some(hello.token.as_str())) {
        let ack = HelloAckPayload {
            ok: false,
            error: Some("Unauthorized: invalid auth token".into()),
            compression: None,
            remote_workdir: None,
        };
        write_json_frame(&mut stream, MsgType::HelloAck, &ack).await?;
        return Err("Unauthorized".into());
    }

    // Pre-flight disk space guard: verify host volume has sufficient free space
    if ctx.min_disk_bytes > 0 {
        if let Ok(space) = workspace::get_disk_space(&ctx.workdir_root) {
            if space.available_bytes < ctx.min_disk_bytes {
                let needed = ctx.min_disk_bytes.saturating_sub(space.available_bytes);
                info!(
                    "Available disk space ({} bytes) is below minimum threshold ({} bytes). Running emergency GC...",
                    space.available_bytes, ctx.min_disk_bytes
                );
                // Snapshot locked projects so emergency GC never deletes
                // workspaces with active runs.
                let locked: std::collections::HashSet<String> = ctx
                    .lock_manager
                    .locked_projects()
                    .await
                    .into_iter()
                    .collect();
                let root = ctx.workdir_root.clone();
                let gc_report = tokio::task::spawn_blocking(move || {
                    workspace::run_emergency_disk_gc(&root, needed, &|name: &str| {
                        locked.contains(name)
                    })
                })
                .await
                .unwrap_or_default();
                info!(
                    "Emergency GC pruned {} workspaces, trimmed {} bytes.",
                    gc_report.workspaces_deleted, gc_report.caches_trimmed_bytes
                );

                if let Ok(new_space) = workspace::get_disk_space(&ctx.workdir_root) {
                    if new_space.available_bytes < ctx.min_disk_bytes {
                        let free_gb = new_space.available_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
                        let req_gb = ctx.min_disk_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
                        let err_msg = format!(
                            "Remote agent disk low ({:.2} GB free, required >= {:.2} GB). Run 'fh clean' or free host disk.",
                            free_gb, req_gb
                        );
                        warn!("{}", err_msg);
                        let ack = HelloAckPayload {
                            ok: false,
                            error: Some(err_msg.clone()),
                            compression: None,
                            remote_workdir: None,
                        };
                        write_json_frame(&mut stream, MsgType::HelloAck, &ack).await?;
                        return Err(err_msg.into());
                    }
                }
            }
        }
    }

    // Negotiate compression algorithm from client's offered list
    let negotiated_compression = if let Some(client_algos) = &hello.compressions {
        if client_algos.iter().any(|a| a.eq_ignore_ascii_case("zstd")) {
            "zstd".to_string()
        } else if client_algos
            .iter()
            .any(|a| a.eq_ignore_ascii_case("gzip") || a.eq_ignore_ascii_case("gz"))
        {
            "gzip".to_string()
        } else if client_algos.iter().any(|a| a.eq_ignore_ascii_case("none")) {
            "none".to_string()
        } else {
            "gzip".to_string()
        }
    } else {
        "gzip".to_string()
    };

    // Resolve persistent workspace directory (forks from seed via APFS CoW if branch)
    let workspace_dir = match workspace::ensure_workspace_dir(&ctx.workdir_root, &hello.project) {
        Ok(dir) => dir,
        Err(e) => {
            let ack = HelloAckPayload {
                ok: false,
                error: Some(format!("Failed to prepare workspace: {}", e)),
                compression: None,
                remote_workdir: None,
            };
            write_json_frame(&mut stream, MsgType::HelloAck, &ack).await?;
            return Err(e.into());
        }
    };
    info!("Using persistent workspace: {}", workspace_dir.display());

    // Acknowledge handshake
    let ack = HelloAckPayload {
        ok: true,
        error: None,
        compression: Some(negotiated_compression.clone()),
        remote_workdir: Some(workspace_dir.display().to_string()),
    };
    write_json_frame(&mut stream, MsgType::HelloAck, &ack).await?;
    info!(
        "Handshake successful for project '{}' (negotiated compression: '{}')",
        hello.project, negotiated_compression
    );

    // Split the stream up front: writes go through the shared writer from
    // here on, and a watchdog can hold the read half while we are queued to
    // detect client disconnects (a dead client must free its queue slot).
    let (read_half, write_half) = tokio::io::split(stream);
    let shared_writer = Arc::new(Mutex::new(write_half));
    let mut read_half = read_half;
    let mut queued_frames: Vec<(MsgType, Vec<u8>)> = Vec::new();

    // 2. Receive next frame: PUT_TEMPLATE or MANIFEST
    let (msg_type, payload) = read_frame(&mut read_half).await?;
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
                    compression: None,
                    remote_workdir: None,
                };
                {
                    let mut w = shared_writer.lock().await;
                    write_json_frame(&mut *w, MsgType::HelloAck, &ack).await?;
                }
                return Ok(());
            }
            Err(e) => {
                warn!("Failed to save template: {}", e);
                let ack = HelloAckPayload {
                    ok: false,
                    error: Some(e.to_string()),
                    compression: None,
                    remote_workdir: None,
                };
                {
                    let mut w = shared_writer.lock().await;
                    write_json_frame(&mut *w, MsgType::HelloAck, &ack).await?;
                }
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

    /// Wait for `acquire` while watching the client connection for disconnects.
    ///
    /// While queued, the daemon normally does not read the socket — a client that
    /// dies while waiting would hold its queue slot forever. The watchdog owns
    /// the read half during the wait, buffers any frames that (against today's
    /// protocol) arrive early, and detects EOF. On acquisition the read half and
    /// any buffered frames are handed back; `Ok(None)` means the client
    /// disconnected and the caller must abort.
    #[allow(clippy::type_complexity)]
    async fn wait_with_disconnect_watch<S, T, Fut>(
        read_half: tokio::io::ReadHalf<S>,
        queued: protocol::QueuedPayload,
        writer: &Arc<Mutex<tokio::io::WriteHalf<S>>>,
        acquire: Fut,
    ) -> Result<Option<(tokio::io::ReadHalf<S>, T, Vec<(MsgType, Vec<u8>)>)>, String>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
        Fut: std::future::Future<Output = T>,
    {
        let (acquired_tx, acquired_rx) = tokio::sync::oneshot::channel::<()>();
        let watchdog: tokio::task::JoinHandle<
            Result<
                (tokio::io::ReadHalf<S>, Vec<(MsgType, Vec<u8>)>, bool),
                std::convert::Infallible,
            >,
        > = tokio::spawn(async move {
            let mut read_half = read_half;
            let mut buffered: Vec<(MsgType, Vec<u8>)> = Vec::new();
            tokio::pin!(acquired_rx);
            loop {
                tokio::select! {
                    _ = &mut acquired_rx => {
                        break Ok((read_half, buffered, false));
                    }
                    res = read_frame(&mut read_half) => match res {
                        Ok(frame) => {
                            // Early frames are a protocol violation today; buffer
                            // a bounded amount and keep consuming so EOF
                            // detection keeps working.
                            if buffered.len() < 16 {
                                buffered.push(frame);
                            }
                            continue;
                        }
                        Err(_) => {
                            break Ok((read_half, buffered, true));
                        }
                    }
                }
            }
        });

        {
            let mut w = writer.lock().await;
            write_json_frame(&mut *w, MsgType::Queued, &queued)
                .await
                .map_err(|e| e.to_string())?;
        }

        let acquired = acquire.await;
        let _ = acquired_tx.send(());
        let (read_half, buffered, disconnected) = match watchdog.await {
            Ok(Ok(t)) => t,
            Ok(Err(infallible)) => match infallible {},
            Err(join_err) => return Err(format!("queue watchdog panicked: {}", join_err)),
        };

        if disconnected {
            return Ok(None);
        }
        Ok(Some((read_half, acquired, buffered)))
    }

    // Acquire per-project lock to ensure serialized execution on the same project workspace
    let project_mutex = ctx.lock_manager.get_lock(&hello.project).await;
    let _project_guard = match project_mutex.clone().try_lock_owned() {
        Ok(guard) => guard,
        Err(_) => {
            info!(
                "Project '{}' is busy. Sending QUEUED frame...",
                hello.project
            );
            // Admission control: a full queue rejects immediately instead of
            // letting disconnected/queued connections grow memory forever.
            if ctx.queue_depth.load(std::sync::atomic::Ordering::SeqCst) >= ctx.max_queued_runs {
                let ack = HelloAckPayload {
                    ok: false,
                    error: Some(format!(
                        "Agent queue is full ({} queued runs). Try again later.",
                        ctx.max_queued_runs
                    )),
                    compression: None,
                    remote_workdir: None,
                };
                let mut w = shared_writer.lock().await;
                write_json_frame(&mut *w, MsgType::HelloAck, &ack).await?;
                return Err("queue full".into());
            }
            ctx.queue_depth
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let pos = ctx.queue_depth.load(std::sync::atomic::Ordering::SeqCst);
            let queued = protocol::QueuedPayload {
                position: pos,
                reason: "project_busy".to_string(),
            };
            let outcome = wait_with_disconnect_watch(
                read_half,
                queued,
                &shared_writer,
                project_mutex.lock_owned(),
            )
            .await?;
            let (rh, guard, frames) = match outcome {
                Some(t) => t,
                None => {
                    ctx.queue_depth
                        .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                    info!("Client disconnected while queued (project busy); freeing slot");
                    return Ok(());
                }
            };
            read_half = rh;
            queued_frames.extend(frames);
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
            if ctx.queue_depth.load(std::sync::atomic::Ordering::SeqCst) >= ctx.max_queued_runs {
                let ack = HelloAckPayload {
                    ok: false,
                    error: Some(format!(
                        "Agent queue is full ({} queued runs). Try again later.",
                        ctx.max_queued_runs
                    )),
                    compression: None,
                    remote_workdir: None,
                };
                let mut w = shared_writer.lock().await;
                write_json_frame(&mut *w, MsgType::HelloAck, &ack).await?;
                return Err("queue full".into());
            }
            ctx.queue_depth
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let pos = ctx.queue_depth.load(std::sync::atomic::Ordering::SeqCst);
            let queued = protocol::QueuedPayload {
                position: pos,
                reason: "concurrency_limit".to_string(),
            };
            let outcome = wait_with_disconnect_watch(
                read_half,
                queued,
                &shared_writer,
                ctx.semaphore.clone().acquire_owned(),
            )
            .await?;
            let (rh, permit_res, frames) = match outcome {
                Some(t) => t,
                None => {
                    ctx.queue_depth
                        .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                    info!("Client disconnected while queued (concurrency limit); freeing slot");
                    return Ok(());
                }
            };
            read_half = rh;
            queued_frames.extend(frames);
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
    // Full-workspace scan + hashing is blocking I/O — keep it off the async
    // runtime (workdir/manifest/ignores moved in owned form).
    let diff_dir = workspace_dir.clone();
    let diff_manifest = manifest.clone();
    let diff_ignores = extra_ignores.clone();
    let mut diff = tokio::task::spawn_blocking(move || {
        workspace::diff_manifests(&diff_dir, &diff_manifest, &diff_ignores)
    })
    .await
    .map_err(|e| -> Box<dyn std::error::Error> { Box::new(std::io::Error::other(e)) })??;

    if let Some(cas) = &ctx.cas_store {
        let manifest_map: std::collections::HashMap<&str, &str> = manifest
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.hash.as_str()))
            .collect();

        let mut remaining_want = Vec::new();
        let mut hydrated_count = 0;

        for rel_path in diff.want {
            if let Some(hash) = manifest_map.get(rel_path.as_str()) {
                if let Ok(rel_buf) = protocol::from_wire_path(&rel_path) {
                    let target_path = workspace_dir.join(rel_buf);
                    if let Ok(true) = cas.materialize_to(hash, &target_path) {
                        hydrated_count += 1;
                        continue;
                    }
                }
            }
            remaining_want.push(rel_path);
        }

        if hydrated_count > 0 {
            info!(
                "Hydrated {} file(s) from global CAS without network transfer",
                hydrated_count
            );
        }
        diff.want = remaining_want;
    }

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
    {
        let mut w = shared_writer.lock().await;
        write_json_frame(&mut *w, MsgType::Need, &need).await?;
    }

    // 4. Receive FILES frame (delta archive). If a queued watchdog buffered
    // early frames, consume them first (defensive; the client sends nothing
    // while queued in today's protocol).
    let (msg_type, payload) = loop {
        if !queued_frames.is_empty() {
            let frame = queued_frames.remove(0);
            if frame.0 == MsgType::Files {
                break frame;
            }
            warn!(
                "Discarding unexpected buffered frame {:?} from queue window",
                frame.0
            );
            continue;
        }
        match read_frame(&mut read_half).await {
            Ok(frame) => break frame,
            Err(e) => {
                // `fh sync --dry-run` reads NEED to learn what would move and
                // then hangs up without sending FILES. Nothing was transferred
                // and the workspace is untouched, which is exactly what the
                // client asked for — not a protocol error.
                if matches!(e, FrameError::Io(_) | FrameError::UnexpectedEof) {
                    info!("Client hung up after NEED (dry run); no files were transferred");
                    return Ok(());
                }
                return Err(e.into());
            }
        }
    };
    if msg_type != MsgType::Files {
        return Err(format!("Expected FILES frame, got {:?}", msg_type).into());
    }

    let bytes_synced = payload.len() as u64;
    if !payload.is_empty() {
        info!("Unpacking {} delta bytes into workspace", payload.len());
        let unpack_dir = workspace_dir.clone();
        let unpack_payload = payload;
        tokio::task::spawn_blocking(move || fileset::unpack_tar(&unpack_dir, &unpack_payload))
            .await
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(std::io::Error::other(e)) })??;

        if let Some(cas) = &ctx.cas_store {
            for entry in &manifest.files {
                if let Ok(rel_buf) = protocol::from_wire_path(&entry.path) {
                    let local_file = workspace_dir.join(rel_buf);
                    if local_file.is_file() {
                        let _ = cas.put_file(&entry.hash, &local_file);
                    }
                }
            }
        }
    } else {
        info!("Zero delta bytes uploaded (workspace up to date)");
    }

    // Apply deletions of extraneous files
    if !diff.delete_extraneous.is_empty() {
        let deleted = workspace::apply_deletions(&workspace_dir, &diff.delete_extraneous)?;
        info!("Deleted {} extraneous files from workspace", deleted);
    }

    // 5. Receive RUN frame
    let (msg_type, payload) = loop {
        if !queued_frames.is_empty() {
            let frame = queued_frames.remove(0);
            if frame.0 == MsgType::Run {
                break frame;
            }
            warn!(
                "Discarding unexpected buffered frame {:?} before RUN",
                frame.0
            );
            continue;
        }
        match read_frame(&mut read_half).await {
            Ok(frame) => break frame,
            Err(e) => {
                // A client that sent its files and then hung up has finished a
                // sync-only session (`fh sync`): the delta is already applied
                // and there is no command to run. That is a successful outcome,
                // not a protocol error — the workspace really is up to date.
                if matches!(e, FrameError::Io(_) | FrameError::UnexpectedEof) {
                    info!("Client completed a sync-only session; workspace is up to date");
                    return Ok(());
                }
                return Err(e.into());
            }
        }
    };
    if msg_type != MsgType::Run {
        return Err(format!("Expected RUN frame, got {:?}", msg_type).into());
    }
    let run: RunPayload = decode_json(&payload)?;
    info!("Executing command: {:?}", run.argv);

    let run_start = std::time::Instant::now();
    let run_id = next_run_id();

    ctx.active_builds
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(
            run_id.clone(),
            (
                hello.project.clone(),
                run.argv.clone(),
                std::time::Instant::now(),
                client_addr.clone(),
            ),
        );
    let _active_build_guard = ActiveBuildGuard {
        active_builds: ctx.active_builds.clone(),
        run_id: run_id.clone(),
    };

    // 6. Pre-build dependency caching hook
    // (the stream was already split after the handshake; read/write halves
    // and the shared writer are in scope)

    let matched_templates = templates::match_templates(&workspace_dir, run.template.as_deref());
    let hook_template = matched_templates
        .into_iter()
        .find(|t| t.hints.install_command.is_some());

    if let Some(template) = hook_template {
        let install_cmd = template.hints.install_command.as_ref().unwrap();
        let current_lock_hash =
            workspace::state::compute_lockfiles_hash(&workspace_dir, &template.hints.lockfiles);
        let prev_state = workspace::state::read_state(&workspace_dir);

        let need_install = if let Some(ref current_hash) = current_lock_hash {
            run.no_cache
                || match &prev_state {
                    Some(s) => s.last_success_lockfile_hash != *current_hash,
                    None => true,
                }
        } else if template.hints.lockfiles.is_empty() {
            run.no_cache || prev_state.is_none()
        } else {
            // Lockfiles were declared in the template, but none exist in the workspace
            false
        };

        if need_install {
            let start_banner = format!(
                "=== [farhand] Running dependency hook: {} ===\n",
                install_cmd
            );
            {
                let mut writer = shared_writer.lock().await;
                let log = protocol::LogPayload {
                    stream: "stdout".into(),
                    data: start_banner,
                };
                let _ = write_json_frame(&mut *writer, MsgType::Log, &log).await;
            }

            let hook_exit = execute_raw_command_and_stream(
                shared_writer.clone(),
                &mut read_half,
                &workspace_dir,
                install_cmd,
                ctx.custom_shell.as_deref(),
                run.env.as_ref(),
                run.toolchain.as_ref(),
            )
            .await?;

            if hook_exit != 0 {
                info!(
                    "Dependency install hook failed with exit code {}",
                    hook_exit
                );
                let result = ResultPayload {
                    exit_code: hook_exit,
                    error: Some(format!(
                        "dependency hook '{}' failed with exit code {}",
                        install_cmd, hook_exit
                    )),
                };
                let record = protocol::RunRecord {
                    id: run_id,
                    timestamp_rfc3339: workspace::format_rfc3339(std::time::SystemTime::now()),
                    project: hello.project.clone(),
                    argv: run.argv.clone(),
                    exit_code: hook_exit,
                    duration_ms: run_start.elapsed().as_millis() as u64,
                    bytes_synced,
                    artifact_size: 0,
                    client_addr: client_addr.clone(),
                    error: result.error.clone(),
                };
                if let Err(e) = tokio::task::spawn_blocking({
                    let dir = workspace_dir.clone();
                    move || workspace::history::save_run(&dir, &record)
                })
                .await
                .map_err(|e| -> Box<dyn std::error::Error> { Box::new(std::io::Error::other(e)) })?
                {
                    warn!("Failed to save run record: {}", e);
                }

                let mut writer = shared_writer.lock().await;
                write_json_frame(&mut *writer, MsgType::Result, &result).await?;
                drop(read_half);
                return Ok(());
            }

            // Install succeeded: record state
            let new_state = workspace::WorkspaceState {
                version: 1,
                last_success_lockfile_hash: current_lock_hash.unwrap_or_default(),
                last_installed_at: std::time::SystemTime::now(),
                template: template.name.clone(),
            };
            if let Err(e) = workspace::state::write_state(&workspace_dir, &new_state) {
                warn!("Failed to write workspace state: {}", e);
            }

            let end_banner =
                "=== [farhand] Dependencies up to date. Proceeding to user command ===\n"
                    .to_string();
            {
                let mut writer = shared_writer.lock().await;
                let log = protocol::LogPayload {
                    stream: "stdout".into(),
                    data: end_banner,
                };
                let _ = write_json_frame(&mut *writer, MsgType::Log, &log).await;
            }
        } else {
            info!(
                "Lockfiles unchanged or not present ({:?}). Skipping dependency install hook '{}'.",
                current_lock_hash, install_cmd
            );
        }
    }

    // 7. Execute user command and stream output
    let exit_code = execute_and_stream(
        shared_writer.clone(),
        &mut read_half,
        &workspace_dir,
        &run.argv,
        ctx.custom_shell.as_deref(),
        run.env.as_ref(),
        run.toolchain.as_ref(),
        run.tty,
        run.cols,
        run.rows,
        run.raw_stdio.unwrap_or(false),
    )
    .await?;

    info!("Command exited with status code {}", exit_code);

    // 8. If command succeeded, resolve artifacts before reporting result & saving history
    let mut artifact_size = 0u64;
    let mut artifact_payload = None;
    if exit_code == 0 {
        let artifact_paths = workspace::resolve_artifact_paths(
            &workspace_dir,
            run.outputs.as_deref(),
            run.template.as_deref(),
        );
        if !artifact_paths.is_empty() {
            info!(
                "Packing {} artifact paths using compression '{}'",
                artifact_paths.len(),
                negotiated_compression
            );
            let algo = fileset::CompressionAlgo::from_str_opt(Some(&negotiated_compression));
            let tar_bytes = fileset::pack_tar_with_algo(&workspace_dir, &artifact_paths, algo)?;
            artifact_size = tar_bytes.len() as u64;
            artifact_payload = Some(tar_bytes);
        }
    }

    let record = protocol::RunRecord {
        id: run_id,
        timestamp_rfc3339: workspace::format_rfc3339(std::time::SystemTime::now()),
        project: hello.project.clone(),
        argv: run.argv.clone(),
        exit_code,
        duration_ms: run_start.elapsed().as_millis() as u64,
        bytes_synced,
        artifact_size,
        client_addr,
        error: if exit_code != 0 {
            Some(format!("command exited with status code {}", exit_code))
        } else {
            None
        },
    };
    if let Err(e) = tokio::task::spawn_blocking({
        let dir = workspace_dir.clone();
        move || workspace::history::save_run(&dir, &record)
    })
    .await
    .map_err(|e| -> Box<dyn std::error::Error> { Box::new(std::io::Error::other(e)) })?
    {
        warn!("Failed to save run record: {}", e);
    }

    // 9. Send RESULT and ARTIFACTS frames
    let result = ResultPayload {
        exit_code,
        error: None,
    };
    {
        let mut writer = shared_writer.lock().await;
        write_json_frame(&mut *writer, MsgType::Result, &result).await?;
        if let Some(tar_gz) = artifact_payload {
            write_frame(&mut *writer, MsgType::Artifacts, &tar_gz).await?;
            info!("Sent ARTIFACTS frame ({} bytes)", tar_gz.len());
        }
    }

    drop(read_half);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_with(token: Option<&str>) -> ServerContext {
        ServerContext {
            expected_token: token.map(str::to_string),
            workdir_root: std::env::temp_dir(),
            custom_shell: None,
            semaphore: Arc::new(tokio::sync::Semaphore::new(1)),
            lock_manager: workspace::WorkspaceLockManager::new(),
            tags: vec![],
            queue_depth: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            max_runs: 1,
            min_disk_bytes: 0,
            cas_store: None,
            start_time: std::time::Instant::now(),
            active_builds: Arc::new(std::sync::Mutex::new(HashMap::new())),
            connection_limiter: Arc::new(tokio::sync::Semaphore::new(UNLIMITED_CONNECTIONS)),
            max_queued_runs: 16,
        }
    }

    #[test]
    fn authorize_accepts_matching_token() {
        let ctx = ctx_with(Some("s3cret"));
        assert!(ctx.authorize(Some("s3cret")));
    }

    #[test]
    fn authorize_rejects_wrong_and_missing_tokens() {
        let ctx = ctx_with(Some("s3cret"));
        assert!(!ctx.authorize(Some("wrong")));
        assert!(!ctx.authorize(Some("")));
        assert!(!ctx.authorize(None));
        // Every single-byte mutation must be rejected.
        for i in 0.."s3cret".len() {
            let mut tampered = "s3cret".to_string();
            let replacement = if i % 2 == 0 { "x" } else { "y" };
            tampered.replace_range(i..i + 1, replacement);
            assert!(
                !ctx.authorize(Some(&tampered)),
                "mutation at byte {i} leaked"
            );
        }
    }

    #[test]
    fn authorize_unauthenticated_mode_allows_all_callers() {
        let ctx = ctx_with(None);
        assert!(ctx.authorize(None));
        assert!(ctx.authorize(Some("anything")));
    }

    #[test]
    fn authorize_treats_empty_configured_token_as_legacy_open() {
        // Direct run_server callers only; the CLI rejects empty tokens.
        let ctx = ctx_with(Some(""));
        assert!(ctx.authorize(Some("any")));
        assert!(ctx.authorize(None));
    }

    #[test]
    fn validate_start_config_requires_token_or_opt_in() {
        assert!(validate_start_config(Some("tok"), false).is_ok());
        assert!(validate_start_config(Some("tok"), true).is_ok());

        let no_token = validate_start_config(None, false).unwrap_err();
        assert!(no_token.contains("--token"));
        assert!(no_token.contains("--allow-unauthenticated"));

        assert!(validate_start_config(None, true).is_ok());

        let empty = validate_start_config(Some("   "), false).unwrap_err();
        assert!(empty.contains("empty string"));
    }

    #[test]
    fn is_exposed_bind_classifies_addresses() {
        assert!(is_exposed_bind("0.0.0.0:9876"));
        assert!(is_exposed_bind("[::]:9876"));
        assert!(is_exposed_bind("10.1.2.3:9876"));
        assert!(is_exposed_bind("192.168.1.10:9876"));
        assert!(!is_exposed_bind("127.0.0.1:9876"));
        assert!(!is_exposed_bind("[::1]:9876"));
        // Unparseable input is treated conservatively as exposed.
        assert!(is_exposed_bind("not-an-address"));
    }

    #[test]
    fn parse_custom_shell_rejects_empty_invocations() {
        // Regression: `--shell " "` panicked the connection task on parts[0].
        assert_eq!(parse_custom_shell("  "), None);
        assert_eq!(parse_custom_shell(""), None);
        assert_eq!(
            parse_custom_shell("/bin/sh -c"),
            Some(vec!["/bin/sh".to_string(), "-c".to_string()])
        );
    }

    #[test]
    fn next_run_id_is_collision_free_under_stress() {
        // Regression: `millis ^ pid` collided for two runs in the same
        // millisecond. Nanos + counter must never repeat within a process.
        let mut ids = std::collections::HashSet::with_capacity(10_000);
        for _ in 0..10_000 {
            let id = next_run_id();
            assert!(ids.insert(id), "run_id collision generated");
        }
        // Distinct lengths/format sanity (16 hex nanos + 4 hex counter).
        assert!(ids.iter().all(|id| id.len() == 20));
    }

    #[tokio::test]
    async fn active_build_guard_drop_removes_entry_synchronously() {
        let ctx = ctx_with(Some("tok"));
        let map = ctx.active_builds.clone();
        map.lock().unwrap().insert(
            "run-1".to_string(),
            (
                "proj".to_string(),
                Vec::new(),
                std::time::Instant::now(),
                "127.0.0.1".to_string(),
            ),
        );

        {
            let _guard = ActiveBuildGuard {
                active_builds: map.clone(),
                run_id: "run-1".to_string(),
            };
            assert!(map.lock().unwrap().contains_key("run-1"));
        }
        // Synchronous removal: no detached task, no window where STATUS sees
        // a finished run as active.
        assert!(!map.lock().unwrap().contains_key("run-1"));
    }
}
