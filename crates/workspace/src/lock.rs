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
}
