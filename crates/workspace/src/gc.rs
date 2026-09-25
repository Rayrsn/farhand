use crate::cow::parse_base_project_name;
use crate::state::{read_state, write_state};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tracing::{debug, info};

#[derive(Debug, Clone, Default)]
pub struct GcReport {
    pub total_workspaces_scanned: usize,
    pub caches_trimmed_bytes: u64,
    pub workspaces_deleted: usize,
    pub workspaces_deleted_bytes: u64,
    pub remaining_disk_bytes: u64,
}

/// Result of a CAS garbage-collection pass.
#[derive(Debug, Clone, Default)]
pub struct CasGcReport {
    pub objects_deleted: usize,
    pub bytes_freed: u64,
    pub remaining_bytes: u64,
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

            // Skip non-workspace infrastructure dirs (CAS storage, state).
            if file_name == "cas" || file_name.starts_with('.') {
                continue;
            }

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
///
/// `skip_locked` decides whether a workspace (by project name) may be
/// deleted or cache-trimmed — the daemon passes its workspace-lock check so
/// active runs are never destroyed underneath themselves.
pub fn run_garbage_collection(
    workspaces_root: &Path,
    max_disk_bytes: Option<u64>,
    ttl: Option<Duration>,
    skip_locked: &dyn Fn(&str) -> bool,
) -> GcReport {
    let mut report = GcReport::default();
    let mut workspaces = scan_workspaces(workspaces_root);
    report.total_workspaces_scanned = workspaces.len();

    let now = SystemTime::now();

    // 1. Evict any non-canonical workspace exceeding TTL
    if let Some(ttl_dur) = ttl {
        workspaces.retain(|ws| {
            if !ws.is_canonical {
                if skip_locked(&ws.name) {
                    debug!("GC: skipping locked workspace {} (active run)", ws.name);
                    return true;
                }
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
                if !ws.is_canonical && !skip_locked(&ws.name) {
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
                if !ws.is_canonical && !skip_locked(&ws.name) {
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

/// Emergency disk cleanup pass: trims caches in non-canonical workspaces,
/// and if needed, evicts oldest non-canonical workspaces until target_bytes_to_free is reached.
///
/// `skip_locked` protects workspaces with active runs (same contract as
/// [`run_garbage_collection`]).
pub fn run_emergency_disk_gc(
    workspaces_root: &Path,
    target_bytes_to_free: u64,
    skip_locked: &dyn Fn(&str) -> bool,
) -> GcReport {
    let mut report = GcReport::default();
    let mut workspaces = scan_workspaces(workspaces_root);
    report.total_workspaces_scanned = workspaces.len();

    // Sort LRU: oldest last_used_at first
    workspaces.sort_by_key(|w| w.last_used_at);

    let mut freed_bytes = 0u64;

    // Phase 1: Trim incremental / compiler caches in non-canonical workspaces
    for ws in &mut workspaces {
        if freed_bytes >= target_bytes_to_free {
            break;
        }
        if !ws.is_canonical && !skip_locked(&ws.name) {
            let trimmed = trim_workspace_caches(&ws.path);
            report.caches_trimmed_bytes += trimmed;
            freed_bytes += trimmed;
            ws.size_bytes = ws.size_bytes.saturating_sub(trimmed);
        }
    }

    // Phase 2: Purge oldest non-canonical workspaces if still below target
    for ws in &workspaces {
        if freed_bytes >= target_bytes_to_free {
            break;
        }
        if !ws.is_canonical && !skip_locked(&ws.name) {
            info!(
                "Emergency GC: Purging LRU workspace {} (size {} bytes)...",
                ws.path.display(),
                ws.size_bytes
            );
            let _ = fs::remove_dir_all(&ws.path);
            report.workspaces_deleted += 1;
            report.workspaces_deleted_bytes += ws.size_bytes;
            freed_bytes += ws.size_bytes;
        }
    }

    report.remaining_disk_bytes = freed_bytes;
    report
}

/// Garbage-collect the content-addressable store.
///
/// CAS is a **cache**: an evicted object is simply re-uploaded on the next
/// manifest mismatch, so retention is a throughput trade, never a correctness
/// one. Policy:
/// 1. TTL — objects whose mtime is older than `ttl` are removed. Hydration
///    touches object mtimes (see `CasStore::materialize_to`), so this means
///    "unused since", not "ingested at".
/// 2. Quota — if the store exceeds `max_bytes`, oldest-mtime objects are
///    evicted first (LRU).
/// 3. Stale `.tmp` files (aborted `put_file` uploads) are removed after an
///    hour regardless of quota.
pub fn gc_cas(
    cas_objects_dir: &Path,
    max_bytes: Option<u64>,
    ttl: Option<Duration>,
) -> CasGcReport {
    let mut report = CasGcReport::default();

    let mut objects: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
    for entry in walkdir::WalkDir::new(cas_objects_dir)
        .follow_links(false)
        .into_iter()
        .flatten()
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        objects.push((
            path.to_path_buf(),
            meta.len(),
            meta.modified().unwrap_or(SystemTime::now()),
        ));
    }

    let now = SystemTime::now();
    let mut remaining: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
    let is_tmp = |p: &Path| {
        p.file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.contains(".tmp."))
            .unwrap_or(false)
    };

    for (path, size, mtime) in objects {
        let age = now.duration_since(mtime).unwrap_or_default();
        let stale_tmp = is_tmp(&path) && age > Duration::from_secs(3600);
        let expired = ttl.map(|t| age > t).unwrap_or(false);

        if stale_tmp || expired {
            if fs::remove_file(&path).is_ok() {
                report.objects_deleted += 1;
                report.bytes_freed += size;
            } else {
                remaining.push((path, size, mtime));
            }
        } else {
            remaining.push((path, size, mtime));
        }
    }

    // Quota: evict oldest-touched first.
    if let Some(max_bytes) = max_bytes {
        let mut total: u64 = remaining.iter().map(|(_, s, _)| *s).sum();
        if total > max_bytes {
            remaining.sort_by_key(|(_, _, mtime)| *mtime);
            let mut survivors = Vec::new();
            for (path, size, mtime) in remaining {
                if total > max_bytes && fs::remove_file(&path).is_ok() {
                    report.objects_deleted += 1;
                    report.bytes_freed += size;
                    total = total.saturating_sub(size);
                } else {
                    survivors.push((path, size, mtime));
                }
            }
            remaining = survivors;
        }
    }

    report.remaining_bytes = remaining.iter().map(|(_, s, _)| *s).sum();
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
        let report = run_garbage_collection(root, Some(1200), None, &|_| false);

        assert_eq!(report.workspaces_deleted, 1);
        assert!(!feat_ws.exists());
        assert!(main_ws.exists(), "Canonical workspace must be preserved");
    }

    #[test]
    fn test_run_garbage_collection_skips_locked_workspaces() {
        let temp = tempdir().unwrap();
        let root = temp.path();

        let feat_ws = root.join("my-repo__feat1-87654321");
        fs::create_dir_all(&feat_ws).unwrap();
        fs::write(feat_ws.join("data.bin"), vec![0u8; 1000]).unwrap();

        // Quota forces eviction, but the workspace's project is locked
        // (an active run holds it) — GC must leave it alone.
        let report = run_garbage_collection(root, Some(500), None, &|name| {
            name.starts_with("my-repo__feat1")
        });

        assert_eq!(report.workspaces_deleted, 0);
        assert!(feat_ws.exists(), "locked workspace must survive GC");
    }

    #[test]
    fn test_run_emergency_disk_gc() {
        let temp = tempdir().unwrap();
        let root = temp.path();

        let feat_ws = root.join("my-repo__feat1-87654321");
        fs::create_dir_all(&feat_ws).unwrap();
        fs::write(feat_ws.join("data.bin"), vec![0u8; 2000]).unwrap();

        let report = run_emergency_disk_gc(root, 1000, &|_| false);
        assert_eq!(report.workspaces_deleted, 1);
        assert!(!feat_ws.exists());
    }

    #[test]
    fn test_run_emergency_disk_gc_skips_locked() {
        let temp = tempdir().unwrap();
        let root = temp.path();

        let feat_ws = root.join("my-repo__feat1-87654321");
        fs::create_dir_all(&feat_ws).unwrap();
        fs::write(feat_ws.join("data.bin"), vec![0u8; 2000]).unwrap();

        let report = run_emergency_disk_gc(root, 1000, &|_| true);
        assert_eq!(report.workspaces_deleted, 0);
        assert!(feat_ws.exists());
    }
}

#[cfg(test)]
mod cas_gc_tests {
    use super::*;
    use std::time::Duration;

    /// Build a fake CAS object layout: cas_objects_dir/ab/cd/<hash>.
    fn make_object(dir: &Path, hash: &str, size: usize, age_secs: u64) -> PathBuf {
        let path = dir.join(&hash[..2]).join(&hash[2..4]).join(hash);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, vec![0u8; size]).unwrap();
        let old = SystemTime::now() - Duration::from_secs(age_secs);
        let f = fs::File::options().write(true).open(&path).unwrap();
        f.set_modified(old).unwrap();
        path
    }

    #[test]
    fn test_gc_cas_ttl_evicts_unused_objects() {
        let dir = tempfile::tempdir().unwrap();
        let fresh = make_object(dir.path(), "aa11fresh_object_1", 100, 0);
        let stale = make_object(dir.path(), "bb22stale_object_2", 100, 40 * 86400);

        let report = gc_cas(dir.path(), None, Some(Duration::from_secs(30 * 86400)));

        assert_eq!(report.objects_deleted, 1);
        assert_eq!(report.bytes_freed, 100);
        assert!(fresh.is_file(), "fresh object must survive");
        assert!(!stale.exists(), "stale object must be evicted");
    }

    #[test]
    fn test_gc_cas_quota_evicts_lru_first() {
        let dir = tempfile::tempdir().unwrap();
        let old = make_object(dir.path(), "cc33old_object_1111", 400, 86400);
        let new = make_object(dir.path(), "dd44new_object_1111", 400, 1);

        // Quota 500 bytes, store has 800 → oldest (old) must go first.
        let report = gc_cas(dir.path(), Some(500), None);

        assert_eq!(report.objects_deleted, 1);
        assert!(!old.exists(), "oldest object evicted under quota");
        assert!(new.is_file(), "newest object survives under quota");
    }

    #[test]
    fn test_gc_cas_removes_stale_tmp_files() {
        let dir = tempfile::tempdir().unwrap();
        let tmp = dir.path().join("ab").join("cd");
        fs::create_dir_all(&tmp).unwrap();
        let tmp_file = tmp.join("abcddeadbeef.tmp.12345.7");
        fs::write(&tmp_file, vec![0u8; 50]).unwrap();
        let old = SystemTime::now() - Duration::from_secs(7200);
        let f = fs::File::options().write(true).open(&tmp_file).unwrap();
        f.set_modified(old).unwrap();

        let report = gc_cas(dir.path(), None, None);

        assert!(!tmp_file.exists(), "stale tmp file must be cleaned");
        assert_eq!(report.objects_deleted, 1);
    }

    #[test]
    fn test_scan_workspaces_skips_cas_dir() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("myrepo")).unwrap();
        fs::create_dir_all(root.path().join("cas").join("objects")).unwrap();

        let scanned = scan_workspaces(root.path());
        let names: Vec<&str> = scanned.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["myrepo"],
            "cas dir must not count as a workspace"
        );
    }
}
