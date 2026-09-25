// Unsafe is allowed only at the FFI boundaries listed in CONTRIBUTING.md
// (gethostname, kill(2), platform resource probes), each with a SAFETY
// contract. Connection handling, sync, execution, and session logic must
// stay pure safe Rust.
#![deny(unsafe_code)]

pub mod metrics;

use protocol::{
    decode_json, read_frame, read_frame_limited, write_frame, write_json_frame, HelloAckPayload,
    HelloPayload, LogPayload, ManifestPayload, MsgType, NeedPayload, PortClosePayload,
    PortDataPayload, PortOpenPayload, ResizePayload, ResultPayload, RunPayload,
    CURRENT_PROTOCOL_VERSION,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

/// Best-effort identity of this agent, used for STATUS reporting and tags.
///
/// The environment wins over `gethostname(2)`: `HOSTNAME`/`COMPUTERNAME` is
/// how operators relabel an agent in a pool (matching the `fhd` tag
/// selection rules), and on most container/VM setups it is already set — so
/// the FFI path is only reached on a bare-metal Unix host with no env var.
#[allow(unsafe_code)] // FFI: gethostname(2) — SAFETY contract inside the body.
pub fn get_hostname() -> String {
    if let Ok(name) = std::env::var("HOSTNAME").or_else(|_| std::env::var("COMPUTERNAME")) {
        if !name.is_empty() {
            return name;
        }
    }

    #[cfg(unix)]
    {
        // POSIX allows gethostname(2) to fill the buffer with no trailing NUL,
        // hence the explicit length scan below.
        let mut buf = [0u8; 256];
        // SAFETY: `buf` is a live, writable array of 256 bytes; the cast to
        // `*mut c_char` is only an aliasing view of the same bytes, and the
        // length passed matches the array bound. gethostname never retains
        // the pointer and writes at most `len` bytes.
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

    "fhd-agent".to_string()
}

pub type ActiveBuildEntry = (String, Vec<String>, std::time::Instant, String);
/// Active builds use a std mutex: guards remove their entries synchronously
/// from `Drop` (no detached task, no runtime-shutdown panic), and critical
/// sections never await.
pub type ActiveBuildMap = Arc<std::sync::Mutex<HashMap<String, ActiveBuildEntry>>>;

/// Monotonic, collision-free run identifier generator.
///
/// The previous scheme (`millis ^ pid & 0xffffffff`) collided whenever two
/// runs started in the same millisecond — pid is constant within a process,
/// so identical timestamps produced identical IDs, clobbering STATUS entries
/// and history file names. Nanosecond timestamp + a process-local counter
/// makes collisions impossible within (and practically across) restarts.
fn next_run_id() -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let counter = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{:016x}{:04x}", nanos, counter & 0xffff)
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
    pub min_disk_bytes: u64,
    pub cas_store: Option<workspace::CasStore>,
    pub start_time: std::time::Instant,
    pub active_builds: ActiveBuildMap,
    /// Caps concurrent client connections; excess sockets are closed on accept.
    pub connection_limiter: Arc<tokio::sync::Semaphore>,
    /// Caps how many runs may wait (project lock or concurrency semaphore)
    /// before new runs are rejected with a queue-full error.
    pub max_queued_runs: usize,
}

impl ServerContext {
    /// Constant-time authorization check for a client-provided token.
    ///
    /// - No token configured (unauthenticated mode): every caller is authorized.
    /// - Token configured: the provided token must match in constant time
    ///   (see [`protocol::ct_eq_tokens`]). Empty configured tokens are treated
    ///   as "no authentication" for compatibility with direct `run_server`
    ///   callers; the CLI rejects them at startup.
    pub fn authorize(&self, provided: Option<&str>) -> bool {
        match self.expected_token.as_deref() {
            None => true,
            // Direct run_server callers only; the CLI rejects empty tokens.
            Some("") => true,
            Some(expected) => protocol::ct_eq_tokens(provided, Some(expected)),
        }
    }
}

/// Validate daemon startup authentication configuration.
///
/// The daemon refuses to start without a token unless unauthenticated mode is
/// explicitly requested: an unauthenticated `fhd` is remote code execution by
/// design. An empty token string is also rejected as a misconfiguration.
pub fn validate_start_config(
    token: Option<&str>,
    allow_unauthenticated: bool,
) -> Result<(), String> {
    match token {
        Some(t) if t.trim().is_empty() => Err(
            "--token was set to an empty string. Set a real token via --token or the \
             FARHAND_TOKEN environment variable, or pass --allow-unauthenticated to \
             intentionally disable authentication."
                .to_string(),
        ),
        Some(_) => Ok(()),
        None if allow_unauthenticated => Ok(()),
        None => Err(
            "No authentication token configured. fhd executes commands sent by \
             authenticated clients, so it refuses to start without a token.\n  \
             Set one:    --token <secret>   (or FARHAND_TOKEN env var)\n  \
             Dev only:   --allow-unauthenticated  (NEVER expose to untrusted networks)"
                .to_string(),
        ),
    }
}

/// Default concurrent-connection cap when none is configured.
const DEFAULT_MAX_CONNECTIONS: usize = 32;
/// "Unlimited" sentinel for the connection limiter (tokio semaphores cap
/// permits well below usize::MAX); a build agent will never approach this.
const UNLIMITED_CONNECTIONS: usize = 1 << 20;

/// Whether a listen address is exposed (unspecified address or any
/// non-loopback IP). Unparseable addresses are treated conservatively as
/// exposed.
pub fn is_exposed_bind(listen: &str) -> bool {
    match listen.parse::<std::net::SocketAddr>() {
        Ok(addr) => !addr.ip().is_loopback(),
        Err(_) => true,
    }
}

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

struct ActiveBuildGuard {
    active_builds: ActiveBuildMap,
    run_id: String,
}

impl Drop for ActiveBuildGuard {
    fn drop(&mut self) {
        // Synchronous removal: STATUS must never observe a finished run as
        // active, and a detached task would panic at runtime shutdown.
        if let Ok(mut map) = self.active_builds.lock() {
            map.remove(&self.run_id);
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
            let ack = HelloAckPayload {
                ok: false,
                error: Some("Unauthorized STATUS request".into()),
                compression: None,
                remote_workdir: None,
            };
            write_json_frame(&mut stream, MsgType::HelloAck, &ack).await?;
            return Err("Unauthorized STATUS request".into());
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
            let ack = HelloAckPayload {
                ok: false,
                error: Some("Unauthorized HISTORY request".into()),
                compression: None,
                remote_workdir: None,
            };
            write_json_frame(&mut stream, MsgType::HelloAck, &ack).await?;
            return Err("Unauthorized HISTORY request".into());
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
            let ack = HelloAckPayload {
                ok: false,
                error: Some("Unauthorized CLEAN request".into()),
                compression: None,
                remote_workdir: None,
            };
            write_json_frame(&mut stream, MsgType::HelloAck, &ack).await?;
            return Err("Unauthorized CLEAN request".into());
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
        break read_frame(&mut read_half).await?;
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
        break read_frame(&mut read_half).await?;
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

#[cfg(windows)]
fn shell_escape(arg: &str) -> String {
    if arg.is_empty() {
        return "\"\"".to_string();
    }
    if arg.chars().all(|c| {
        c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | '\\' | ':' | '=' | '@')
    }) {
        return arg.to_string();
    }
    format!("\"{}\"", arg.replace('"', "\\\""))
}

#[cfg(not(windows))]
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

pub fn wrap_command_with_toolchain(
    cmd_str: &str,
    toolchain: Option<&HashMap<String, String>>,
) -> String {
    let Some(toolchain) = toolchain else {
        return cmd_str.to_string();
    };
    if toolchain.is_empty() {
        return cmd_str.to_string();
    }

    #[cfg(not(unix))]
    {
        let _ = toolchain;
        cmd_str.to_string()
    }

    #[cfg(unix)]
    {
        let mut prefixes: Vec<String> = Vec::new();
        for (lang, ver) in toolchain {
            let l = lang.to_ascii_lowercase();
            match l.as_str() {
                "node" | "nodejs" => {
                    prefixes.push(format!(
                        "(export NVM_DIR=\"$HOME/.nvm\"; [ -s \"$NVM_DIR/nvm.sh\" ] && \\. \"$NVM_DIR/nvm.sh\" && nvm use {} >/dev/null 2>&1) || (which fnm >/dev/null 2>&1 && eval \"$(fnm env)\" && fnm use {} >/dev/null 2>&1) || true",
                        ver, ver
                    ));
                }
                "go" | "golang" => {
                    prefixes.push(format!(
                        "(which goenv >/dev/null 2>&1 && export GOENV_VERSION={} && eval \"$(goenv init -)\") || true",
                        ver
                    ));
                }
                "python" | "pyenv" => {
                    prefixes.push(
                        "(which pyenv >/dev/null 2>&1 && eval \"$(pyenv init -)\") || true"
                            .to_string(),
                    );
                }
                _ => {}
            }
        }

        if prefixes.is_empty() {
            cmd_str.to_string()
        } else {
            format!("{} && {}", prefixes.join(" && "), cmd_str)
        }
    }
}

pub fn apply_toolchain_env(cmd: &mut Command, toolchain: Option<&HashMap<String, String>>) {
    if let Some(tc) = toolchain {
        for (lang, ver) in tc {
            let l = lang.to_ascii_lowercase();
            match l.as_str() {
                "rust" | "rustup" => {
                    cmd.env("RUSTUP_TOOLCHAIN", ver);
                }
                "python" | "pyenv" => {
                    cmd.env("PYENV_VERSION", ver);
                }
                "node" | "nodejs" => {
                    cmd.env("NODE_VERSION", ver);
                }
                _ => {}
            }
            let env_key = format!("FARHAND_TOOLCHAIN_{}", l.to_ascii_uppercase());
            cmd.env(env_key, ver);
        }
    }
}

pub fn apply_toolchain_pty(
    cmd_builder: &mut portable_pty::CommandBuilder,
    toolchain: Option<&HashMap<String, String>>,
) {
    if let Some(tc) = toolchain {
        for (lang, ver) in tc {
            let l = lang.to_ascii_lowercase();
            match l.as_str() {
                "rust" | "rustup" => {
                    cmd_builder.env("RUSTUP_TOOLCHAIN", ver);
                }
                "python" | "pyenv" => {
                    cmd_builder.env("PYENV_VERSION", ver);
                }
                "node" | "nodejs" => {
                    cmd_builder.env("NODE_VERSION", ver);
                }
                _ => {}
            }
            let env_key = format!("FARHAND_TOOLCHAIN_{}", l.to_ascii_uppercase());
            cmd_builder.env(env_key, ver);
        }
    }
}

/// Parse a custom shell invocation (e.g. `/bin/sh -c`) into argv tokens.
/// Returns `None` for empty or whitespace-only input — a misconfiguration the
/// daemon rejects at startup; library callers fall back to the default shell.
pub fn parse_custom_shell(shell: &str) -> Option<Vec<String>> {
    let tokens: Vec<String> = shell.split_whitespace().map(str::to_string).collect();
    if tokens.is_empty() {
        None
    } else {
        Some(tokens)
    }
}

pub fn build_shell_command(
    cwd: &Path,
    argv: &[String],
    custom_shell: Option<&str>,
    toolchain: Option<&HashMap<String, String>>,
) -> Command {
    let joined_cmd = argv
        .iter()
        .map(|a| shell_escape(a))
        .collect::<Vec<_>>()
        .join(" ");
    let wrapped_cmd = wrap_command_with_toolchain(&joined_cmd, toolchain);

    let mut cmd = if let Some(shell_override) = custom_shell {
        let shell_tokens = parse_custom_shell(shell_override)
            .unwrap_or_else(|| vec!["/bin/sh".to_string(), "-c".to_string()]);
        let mut c = Command::new(&shell_tokens[0]);
        for part in &shell_tokens[1..] {
            c.arg(part);
        }
        c.arg(&wrapped_cmd);
        c
    } else if cfg!(windows) {
        let mut c = Command::new("cmd.exe");
        #[cfg(windows)]
        c.raw_arg(format!("/C \"{}\"", wrapped_cmd));
        #[cfg(not(windows))]
        c.arg("/C").arg(&wrapped_cmd);
        c
    } else {
        let mut c = Command::new("/bin/sh");
        c.arg("-c").arg(&wrapped_cmd);
        c
    };

    cmd.current_dir(cwd);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    #[cfg(unix)]
    cmd.process_group(0);

    cmd
}

pub fn build_raw_shell_command(
    cwd: &Path,
    raw_cmd: &str,
    custom_shell: Option<&str>,
    toolchain: Option<&HashMap<String, String>>,
) -> Command {
    let wrapped_cmd = wrap_command_with_toolchain(raw_cmd, toolchain);
    let mut cmd = if let Some(shell_override) = custom_shell {
        let shell_tokens = parse_custom_shell(shell_override)
            .unwrap_or_else(|| vec!["/bin/sh".to_string(), "-c".to_string()]);
        let mut c = Command::new(&shell_tokens[0]);
        for part in &shell_tokens[1..] {
            c.arg(part);
        }
        c.arg(&wrapped_cmd);
        c
    } else if cfg!(windows) {
        let mut c = Command::new("cmd.exe");
        #[cfg(windows)]
        c.raw_arg(format!("/C \"{}\"", wrapped_cmd));
        #[cfg(not(windows))]
        c.arg("/C").arg(&wrapped_cmd);
        c
    } else {
        let mut c = Command::new("/bin/sh");
        c.arg("-c").arg(&wrapped_cmd);
        c
    };

    cmd.current_dir(cwd);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    #[cfg(unix)]
    cmd.process_group(0);

    cmd
}

/// Terminate a remote command's whole process group (AGENTS.md §3.4).
///
/// SIGTERM first, then SIGKILL after 3s: the negative pid signals the
/// process *group*, which is why `build_command` sets `process_group(0)` —
/// that makes the child a group leader whose pgid equals its pid, so the
/// compiler tree (rustc → linker → build script) dies together instead of
/// leaving orphans behind.
#[allow(unsafe_code)] // FFI: kill(2) on our own child's process group — SAFETY comments inside.
pub async fn kill_process_group(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // SAFETY: `pid` is the id of a child this daemon spawned with
        // `process_group(0)`, so `-pid` addresses that child's process group
        // and can only signal processes we own. `kill` is async-signal-safe
        // and has no memory preconditions.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGTERM);
        }
        tokio::select! {
            _ = child.wait() => {}
            _ = tokio::time::sleep(std::time::Duration::from_secs(3)) => {
                // SAFETY: as above; the process group may still contain
                // descendants that ignored SIGTERM.
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
            }
        }
    }
    #[cfg(windows)]
    if let Some(pid) = child.id() {
        let _ = tokio::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .output()
            .await;
        let _ = child.kill().await;
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = child.kill().await;
    }
}

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

pub fn resolve_shell_executable(requested: &str) -> String {
    if requested != "$SHELL" && !requested.is_empty() {
        if Path::new(requested).is_file() {
            return requested.to_string();
        }
        if !requested.contains('/') && !requested.contains('\\') {
            for dir in &["/bin", "/usr/bin", "/usr/local/bin", "/opt/homebrew/bin"] {
                let candidate = Path::new(dir).join(requested);
                if candidate.is_file() {
                    return candidate.to_string_lossy().to_string();
                }
            }
        }
    }

    if let Ok(sh) = std::env::var("SHELL") {
        if Path::new(&sh).is_file() {
            return sh;
        }
    }
    for candidate in &[
        "/bin/zsh",
        "/bin/bash",
        "/usr/bin/zsh",
        "/usr/bin/bash",
        "/bin/sh",
    ] {
        if Path::new(candidate).is_file() {
            return candidate.to_string();
        }
    }
    #[cfg(windows)]
    {
        "powershell.exe".to_string()
    }
    #[cfg(not(windows))]
    {
        "/bin/sh".to_string()
    }
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
