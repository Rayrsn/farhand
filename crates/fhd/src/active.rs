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

/// Releases a queue slot on drop, however the waiting run leaves.
///
/// The queue depth is what decides whether a new run is accepted, so a leaked
/// increment is permanent: after `max_queued_runs` of them the agent answers
/// "queue is full" to everything, forever, with no way to recover except a
/// restart. Decrementing on the success path alone was not enough, because
/// `wait_with_disconnect_watch(...).await?` returns early on error and never
/// reached it.
pub(crate) struct QueueSlotGuard {
    depth: Arc<std::sync::atomic::AtomicUsize>,
}

impl QueueSlotGuard {
    pub(crate) fn take(depth: &Arc<std::sync::atomic::AtomicUsize>) -> Self {
        depth.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Self {
            depth: Arc::clone(depth),
        }
    }
}

impl Drop for QueueSlotGuard {
    fn drop(&mut self) {
        self.depth.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
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

#[cfg(test)]
mod queue_slot_tests {
    use super::*;
    use std::sync::atomic::Ordering;

    /// The queue depth gates whether a new run is accepted, so a leaked
    /// increment is permanent and unrecoverable without a restart. This pins
    /// the property the guard exists to provide: the slot comes back no matter
    /// how the waiting scope leaves.
    #[test]
    fn a_dropped_queue_slot_always_releases_its_depth() {
        let depth = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for _ in 0..10 {
            let _slot = QueueSlotGuard::take(&depth);
            assert_eq!(depth.load(Ordering::SeqCst), 1);
        }
        assert_eq!(depth.load(Ordering::SeqCst), 0, "slots leaked");
    }

    #[test]
    fn a_queue_slot_ignores_early_returns_and_panics() {
        let depth = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        // The shape that used to leak: an early `?` after the increment, before
        // the decrement on the success path.
        fn returns_early(depth: &Arc<std::sync::atomic::AtomicUsize>) -> Result<(), ()> {
            let _slot = QueueSlotGuard::take(depth);
            Err(())
        }
        let _ = returns_early(&depth);
        assert_eq!(depth.load(Ordering::SeqCst), 0);

        let result = std::panic::catch_unwind(|| {
            let _slot = QueueSlotGuard::take(&depth);
            panic!("boom");
        });
        assert!(result.is_err());
        assert_eq!(
            depth.load(Ordering::SeqCst),
            0,
            "a panicking run leaked a slot"
        );
    }
}
