//! Daemon configuration, startup validation, and agent identity.

use crate::active::ActiveBuildMap;
use protocol::{write_json_frame, HelloAckPayload, MsgType};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::AsyncWrite;

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

/// How long the agent waits for a client at each stage of the protocol.
///
/// The connection limiter is acquired *before* the TLS handshake, so without
/// these a client that connects and then stalls holds a permit indefinitely.
/// With `--max-connections 32`, 32 stalled handshakes are enough to make the
/// agent permanently refuse legitimate work.
#[derive(Debug, Clone, Copy)]
pub struct Timeouts {
    /// TLS handshake and the pre-authentication first frame.
    pub handshake: std::time::Duration,
    /// Each subsequent frame before the command is dispatched.
    ///
    /// Generous by default because the FILES frame is a delta upload: a client
    /// on a slow link legitimately takes minutes to deliver one. Note this
    /// never covers the run itself — once a command starts, the connection is
    /// allowed to be silent for as long as the build takes.
    pub io: std::time::Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            handshake: std::time::Duration::from_secs(30),
            io: std::time::Duration::from_secs(300),
        }
    }
}

#[derive(Clone)]
pub struct ServerContext {
    pub timeouts: Timeouts,
    /// Ports a client may ask the agent to forward to via `-L`. Empty refuses
    /// every request: an authenticated client could otherwise reach any TCP
    /// service on the agent's loopback, which is a much larger blast radius
    /// than "run this build command".
    pub forward_allowlist: Vec<u16>,
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

/// Tokens that appear in this repository's own service templates and
/// documentation. Shipping one of these in a formula or a plist that a user
/// installs without editing leaves a publicly known token guarding a daemon
/// that executes arbitrary commands, so the daemon refuses them outright. The
/// check lives here rather than in the templates because it then covers every
/// deployment path — formula, launchd, a hand-written unit, a copy-paste from
/// the README — instead of only the files in this repository.
const PLACEHOLDER_TOKENS: &[&str] = &[
    "replace-with-your-token",
    "replace-with-your-secret-token",
    "replace-me",
    "changeme",
    "your-token-here",
    "token",
];

/// Validate daemon startup authentication configuration.
///
/// The daemon refuses to start without a token unless unauthenticated mode is
/// explicitly requested: an unauthenticated `fhd` is remote code execution by
/// design. An empty token string is also rejected as a misconfiguration, as is
/// any token this repository ships as a documentation placeholder.
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
        Some(t)
            if PLACEHOLDER_TOKENS
                .iter()
                .any(|p| p.eq_ignore_ascii_case(t.trim())) =>
        {
            Err(format!(
                "Refusing to start: the configured token is the placeholder {t:?}.\n  \
                 That exact value is published in this repository's Homebrew formula and \
                 launchd template, so anyone can read it. An fhd token guards remote \
                 command execution.\n  \
                 Generate a real one:  openssl rand -hex 32"
            ))
        }
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
pub(crate) const DEFAULT_MAX_CONNECTIONS: usize = 32;
/// "Unlimited" sentinel for the connection limiter (tokio semaphores cap
/// permits well below usize::MAX); a build agent will never approach this.
pub(crate) const UNLIMITED_CONNECTIONS: usize = 1 << 20;

/// Whether a listen address is exposed (unspecified address or any
/// non-loopback IP). Unparseable addresses are treated conservatively as
/// exposed.
pub fn is_exposed_bind(listen: &str) -> bool {
    match listen.parse::<std::net::SocketAddr>() {
        Ok(addr) => !addr.ip().is_loopback(),
        Err(_) => true,
    }
}

/// Reply to an unauthorized pre-HELLO control request and return the error
/// that closes the connection.
///
/// STATUS, HISTORY, and CLEAN all arrive before authentication and must fail
/// in exactly the same shape, so the rejection lives in one place: a failure
/// `HELLO_ACK` followed by an error that unwinds the connection. Sharing it
/// keeps the three paths from drifting apart as they are edited.
pub(crate) async fn deny_control_request<W: AsyncWrite + Unpin>(
    stream: &mut W,
    request: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let message = format!("Unauthorized {request} request");
    let ack = HelloAckPayload {
        ok: false,
        error: Some(message.clone()),
        compression: None,
        remote_workdir: None,
    };
    write_json_frame(stream, MsgType::HelloAck, &ack).await?;
    Err(message.into())
}
