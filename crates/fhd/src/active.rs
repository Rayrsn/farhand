//! In-memory registry of builds currently running on this agent.
//!
//! The map is the agent's view of "what is busy" for STATUS reporting and
//! project-lock accounting. Entries are removed by `ActiveBuildGuard`'s
//! `Drop`, so a run that is cancelled, panics, or loses its connection can
//! never leak a slot.

use std::collections::HashMap;
use std::sync::Arc;

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
pub(crate) fn next_run_id() -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let counter = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{:016x}{:04x}", nanos, counter & 0xffff)
}

pub(crate) struct ActiveBuildGuard {
    pub(crate) active_builds: ActiveBuildMap,
    pub(crate) run_id: String,
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
