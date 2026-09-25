use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Manages per-project mutex locks to ensure concurrent runs for the same project
/// are safely serialized without corrupting the persistent remote workspace.
#[derive(Clone, Default)]
pub struct WorkspaceLockManager {
    locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
}

impl WorkspaceLockManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Retrieve the shared mutex for the given project name.
    pub async fn get_lock(&self, project: &str) -> Arc<Mutex<()>> {
        let mut map = self.locks.lock().await;
        map.entry(project.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// Whether a run currently holds this project's lock.
    ///
    /// Non-inserting: projects with no lock entry are never locked. Used by
    /// GC and CLEAN paths to avoid deleting workspaces that are in use.
    pub async fn is_locked(&self, project: &str) -> bool {
        let map = self.locks.lock().await;
        map.get(project)
            .map(|m| m.try_lock().is_err())
            .unwrap_or(false)
    }

    /// Snapshot of project names whose locks are currently held. Callers
    /// building synchronous predicates (e.g. GC filters) use this to avoid
    /// holding the manager's map across blocking work.
    pub async fn locked_projects(&self) -> Vec<String> {
        let map = self.locks.lock().await;
        map.iter()
            .filter(|(_, mutex)| mutex.try_lock().is_err())
            .map(|(name, _)| name.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_workspace_lock_manager_same_project_shares_mutex() {
        let manager = WorkspaceLockManager::new();
        let lock1 = manager.get_lock("my-project").await;
        let lock2 = manager.get_lock("my-project").await;

        // Acquire lock1
        let guard1 = lock1.try_lock();
        assert!(guard1.is_ok());

        // lock2 should fail because it references the same underlying project mutex
        let guard2 = lock2.try_lock();
        assert!(guard2.is_err());

        // Drop guard1 and verify lock2 can now acquire
        drop(guard1);
        let guard2_retry = lock2.try_lock();
        assert!(guard2_retry.is_ok());
    }

    #[tokio::test]
    async fn test_workspace_lock_manager_different_projects_independent() {
        let manager = WorkspaceLockManager::new();
        let lock1 = manager.get_lock("project-a").await;
        let lock2 = manager.get_lock("project-b").await;

        let guard1 = lock1.try_lock();
        let guard2 = lock2.try_lock();

        assert!(guard1.is_ok());
        assert!(guard2.is_ok());
    }

    #[tokio::test]
    async fn test_is_locked_reflects_held_locks_without_inserting() {
        let manager = WorkspaceLockManager::new();

        // Unknown projects are never locked.
        assert!(!manager.is_locked("ghost-project").await);

        let lock = manager.get_lock("busy-project").await;
        let guard = lock.try_lock().unwrap();
        assert!(manager.is_locked("busy-project").await);

        // GC snapshots must see the held lock and nothing else.
        let locked = manager.locked_projects().await;
        assert_eq!(locked, vec!["busy-project".to_string()]);

        drop(guard);
        assert!(!manager.is_locked("busy-project").await);
        assert!(manager.locked_projects().await.is_empty());
    }
}
