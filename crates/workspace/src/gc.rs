use crate::cow::parse_base_project_name;
use crate::state::{read_state, write_state};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tracing::info;

#[derive(Debug, Clone, Default)]
pub struct GcReport {
    pub total_workspaces_scanned: usize,
    pub caches_trimmed_bytes: u64,
    pub workspaces_deleted: usize,
    pub workspaces_deleted_bytes: u64,
    pub remaining_disk_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct WorkspaceMetadata {
    pub path: PathBuf,
    pub name: String,
    pub last_used_at: SystemTime,
    pub size_bytes: u64,
    pub is_canonical: bool,
}

/// Calculate total size of a directory recursively in bytes.
pub fn calculate_dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if meta.is_dir() {
                    total += calculate_dir_size(&entry.path());
                } else {
                    total += meta.len();
                }
            }
        }
    }
    total
}

/// Update workspace `last_used_at` timestamp.
pub fn touch_workspace(workspace_dir: &Path, _project_name: &str) {
    if let Some(mut state) = read_state(workspace_dir) {
        state.last_installed_at = SystemTime::now();
        let _ = write_state(workspace_dir, &state);
    }

    // Write / update .farhand-last-used marker file
    let stamp = workspace_dir.join(".farhand-last-used");
    let _ = fs::write(stamp, format!("{:?}", SystemTime::now()));
}

/// Retrieve last used timestamp for a workspace.
pub fn get_workspace_last_used(workspace_dir: &Path) -> SystemTime {
    if let Ok(meta) = fs::metadata(workspace_dir.join(".farhand-last-used")) {
        if let Ok(mtime) = meta.modified() {
            return mtime;
        }
    }

    if let Some(state) = read_state(workspace_dir) {
        return state.last_installed_at;
    }

    if let Ok(meta) = fs::metadata(workspace_dir.join(".farhand-state.json")) {
        if let Ok(mtime) = meta.modified() {
            return mtime;
        }
    }

    if let Ok(meta) = fs::metadata(workspace_dir) {
        if let Ok(mtime) = meta.modified() {
            return mtime;
        }
    }

    SystemTime::UNIX_EPOCH
}

/// Soft pruning: trims volatile compiler caches (e.g. incremental caches, node_modules/.cache).
/// Keeps installed dependencies and build artifacts intact.
pub fn trim_workspace_caches(workspace_dir: &Path) -> u64 {
    let cache_dirs = [
        "target/debug/incremental",
        "target/release/incremental",
        "node_modules/.cache",
        ".next/cache",
        "DerivedData",
        ".gradle/caches",
    ];

    let mut freed_bytes = 0u64;
    for rel_dir in cache_dirs {
        let dir = workspace_dir.join(rel_dir);
        if dir.is_dir() {
            let size = calculate_dir_size(&dir);
            if fs::remove_dir_all(&dir).is_ok() {
                freed_bytes += size;
                info!("Trimmed cache {} (freed {} bytes)", dir.display(), size);
            }
        }
    }

    freed_bytes
}

/// Scan all workspace directories under `workspaces_root`.
pub fn scan_workspaces(workspaces_root: &Path) -> Vec<WorkspaceMetadata> {
    let mut workspaces = Vec::new();

    let entries = match fs::read_dir(workspaces_root) {
        Ok(e) => e,
        Err(_) => return workspaces,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let file_name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();

            let last_used = get_workspace_last_used(&path);
            let size = calculate_dir_size(&path);

            // A workspace is canonical if it does not have branch separators in its name
            // or explicitly matches main/master
            let is_canonical = parse_base_project_name(&file_name).is_none()
                || file_name.contains("__main")
                || file_name.contains("__master")
                || file_name.contains(":main")
                || file_name.contains(":master");

            workspaces.push(WorkspaceMetadata {
                path,
                name: file_name,
                last_used_at: last_used,
                size_bytes: size,
                is_canonical,
            });
        }
    }

    workspaces
}

/// Execute automated Garbage Collection across `workspaces_root`.
pub fn run_garbage_collection(
    workspaces_root: &Path,
    max_disk_bytes: Option<u64>,
    ttl: Option<Duration>,
) -> GcReport {
    let mut report = GcReport::default();
    let mut workspaces = scan_workspaces(workspaces_root);
    report.total_workspaces_scanned = workspaces.len();

    let now = SystemTime::now();

    // 1. Evict any non-canonical workspace exceeding TTL
    if let Some(ttl_dur) = ttl {
        workspaces.retain(|ws| {
            if !ws.is_canonical {
                if let Ok(age) = now.duration_since(ws.last_used_at) {
                    if age > ttl_dur {
                        info!(
                            "TTL expired for workspace {} (age {:?} > {:?}). Purging...",
                            ws.path.display(),
                            age,
                            ttl_dur
                        );
                        let _ = fs::remove_dir_all(&ws.path);
                        report.workspaces_deleted += 1;
                        report.workspaces_deleted_bytes += ws.size_bytes;
                        return false;
                    }
                }
            }
            true
        });
    }

    // 2. Check total storage quota
    let current_total: u64 = workspaces.iter().map(|w| w.size_bytes).sum();
    let mut current_usage = current_total;

    if let Some(max_bytes) = max_disk_bytes {
        if current_usage > max_bytes {
            // Sort workspaces oldest first (LRU)
            workspaces.sort_by_key(|a| a.last_used_at);

            // Tier 1: Soft trim caches of non-canonical workspaces
            for ws in &mut workspaces {
                if current_usage <= max_bytes {
                    break;
                }
                if !ws.is_canonical {
                    let trimmed = trim_workspace_caches(&ws.path);
                    report.caches_trimmed_bytes += trimmed;
                    current_usage = current_usage.saturating_sub(trimmed);
                    ws.size_bytes = ws.size_bytes.saturating_sub(trimmed);
                }
            }

            // Tier 2: Hard eviction of non-canonical workspaces (oldest first)
            for ws in &workspaces {
                if current_usage <= max_bytes {
                    break;
                }
                if !ws.is_canonical {
                    info!(
                        "Quota exceeded. Purging LRU workspace {} (size {} bytes)...",
                        ws.path.display(),
                        ws.size_bytes
                    );
                    let _ = fs::remove_dir_all(&ws.path);
                    report.workspaces_deleted += 1;
                    report.workspaces_deleted_bytes += ws.size_bytes;
                    current_usage = current_usage.saturating_sub(ws.size_bytes);
                }
            }
        }
    }

    report.remaining_disk_bytes = current_usage;
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_trim_workspace_caches() {
        let temp = tempdir().unwrap();
        let ws = temp.path().join("my-ws");

        let cache_dir = ws.join("target/debug/incremental");
        fs::create_dir_all(&cache_dir).unwrap();
        fs::write(cache_dir.join("cache.dat"), "1234567890").unwrap();

        let src_file = ws.join("src/main.rs");
        fs::create_dir_all(ws.join("src")).unwrap();
        fs::write(&src_file, "fn main() {}").unwrap();

        let freed = trim_workspace_caches(&ws);
        assert!(freed >= 10);
        assert!(!cache_dir.exists());
        assert!(src_file.exists());
    }

    #[test]
    fn test_run_garbage_collection_quota_eviction() {
        let temp = tempdir().unwrap();
        let root = temp.path();

        // 1. Canonical workspace (should NOT be deleted)
        let main_ws = root.join("my-repo__main-12345678");
        fs::create_dir_all(&main_ws).unwrap();
        fs::write(main_ws.join("data.bin"), vec![0u8; 1000]).unwrap();

        // 2. Old feature branch workspace
        let feat_ws = root.join("my-repo__feat1-87654321");
        fs::create_dir_all(&feat_ws).unwrap();
        fs::write(feat_ws.join("data.bin"), vec![0u8; 1000]).unwrap();

        // Quota is 1200 bytes, total is ~2000 bytes
        let report = run_garbage_collection(root, Some(1200), None);

        assert_eq!(report.workspaces_deleted, 1);
        assert!(!feat_ws.exists());
        assert!(main_ws.exists(), "Canonical workspace must be preserved");
    }
}
