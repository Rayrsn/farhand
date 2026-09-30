// Unsafe is allowed only at the FFI boundaries listed in CONTRIBUTING.md
// (CoW syscalls, statvfs), each with a SAFETY contract. Everything else —
// locking, CAS, history, GC, presets — must stay pure safe Rust.
#![deny(unsafe_code)]

use fileset::FilesetError;
use protocol::{FileEntry, ManifestPayload};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::debug;

pub mod presets;
pub use presets::{detect_preset_outputs, resolve_artifact_paths, Preset, DEFAULT_PRESETS};

pub mod lock;
pub use lock::WorkspaceLockManager;

pub mod state;
pub use state::{compute_lockfiles_hash, read_state, write_state, WorkspaceState};

pub mod history;
pub use history::{format_rfc3339, get_recent_runs, save_run, MAX_RUNS_RETAINED, RUNS_DIR};

pub mod cow;
pub use cow::{cow_clone_dir, cow_clone_file, find_seed_workspace, parse_base_project_name};

pub mod cas;
pub use cas::CasStore;

pub mod gc;
pub use gc::{
    calculate_dir_size, gc_cas, get_workspace_last_used, run_emergency_disk_gc,
    run_garbage_collection, scan_workspaces, touch_workspace, trim_workspace_caches, CasGcReport,
    GcReport, WorkspaceMetadata,
};

pub mod disk;
pub use disk::{get_disk_space, DiskSpace};

pub mod change_token;
pub use change_token::change_token;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffResult {
    /// List of forward-slash relative paths the agent needs the client to upload
    pub want: Vec<String>,
    /// List of forward-slash relative paths present remotely that should be deleted
    pub delete_extraneous: Vec<String>,
}

/// Resolve a persistent workspace directory path based on a base root and project name.
/// Example: `~/.farhand/workspaces/my-app-8a4b2e1f/`
pub fn resolve_workspace_dir(base_dir: &Path, project_name: &str) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(project_name.as_bytes());
    let hash = hasher.finalize();
    let short_hash = hex::encode(&hash[..4]); // 8 hex characters

    let clean_name: String = project_name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();

    base_dir.join(format!("{}-{}", clean_name, short_hash))
}

/// Ensure a persistent workspace exists for `project_name`.
/// If the directory does not exist, but an existing base/seed workspace exists
/// (e.g. for `repo:main` when creating `repo:feat`), clones it via APFS CoW.
pub fn ensure_workspace_dir(base_dir: &Path, project_name: &str) -> std::io::Result<PathBuf> {
    let ws_dir = resolve_workspace_dir(base_dir, project_name);
    if ws_dir.is_dir() {
        touch_workspace(&ws_dir, project_name);
        return Ok(ws_dir);
    }

    if let Some(seed_dir) = find_seed_workspace(base_dir, project_name) {
        tracing::info!(
            "Forking new branch workspace for '{}' from seed '{}' via APFS CoW...",
            project_name,
            seed_dir.display()
        );
        if let Err(e) = cow_clone_dir(&seed_dir, &ws_dir) {
            tracing::warn!("CoW clone failed ({}); creating clean directory", e);
            fs::create_dir_all(&ws_dir)?;
        }
    } else {
        fs::create_dir_all(&ws_dir)?;
    }

    touch_workspace(&ws_dir, project_name);
    Ok(ws_dir)
}

/// Returns the default workspaces root directory (`~/.farhand/workspaces`).
pub fn default_workspaces_dir() -> PathBuf {
    if let Ok(override_dir) = std::env::var("FARHAND_WORKDIR") {
        return PathBuf::from(override_dir);
    }

    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        PathBuf::from(home).join(".farhand").join("workspaces")
    } else {
        std::env::temp_dir().join("farhand").join("workspaces")
    }
}

/// Diff client manifest against the remote workspace files, obeying Section 5.1 deletion safety.
pub fn diff_manifests(
    workspace_root: &Path,
    client_manifest: &ManifestPayload,
    extra_ignores: &[String],
) -> Result<DiffResult, FilesetError> {
    let mut client_map: HashMap<&str, &FileEntry> =
        HashMap::with_capacity(client_manifest.files.len());
    for f in &client_manifest.files {
        client_map.insert(&f.path, f);
    }

    // The digest index lives in the workspace root, so it must be excluded from
    // the walk or the scan would hash it and the diff would treat it as a
    // stray file. `fileset::scan` already skips ignored names, and the
    // Section 5.1 delete guard below independently skips `.farhand-*`, so the
    // index cannot be deleted even if this exclusion were dropped.
    let index_path = workspace_root.join(fileset::HASH_INDEX_FILENAME);
    let mut ignores = extra_ignores.to_vec();
    ignores.push(fileset::HASH_INDEX_FILENAME.to_string());

    let remote_files = if workspace_root.exists() {
        let mut cache = fileset::load_hash_cache(&index_path);
        let (files, stats) =
            fileset::scan_cached_with(workspace_root, &ignores, &mut cache, &change_token)?;

        // A failed index write costs a full re-hash next run and nothing else,
        // so it must not fail the diff.
        if let Err(e) = fileset::save_hash_cache(&index_path, &cache) {
            debug!("Could not write digest index: {e}");
        }

        debug!(
            "Workspace scan reused {} of {} digests ({} re-hashed)",
            stats.reused,
            stats.reused + stats.hashed,
            stats.hashed
        );

        files
    } else {
        HashMap::new()
    };

    let mut want = Vec::new();
    for (rel_path, client_entry) in &client_map {
        // A path the agent's own ignore rules exclude is not a file the agent
        // will ever scan, so it must never be requested. The client scans with
        // no extra ignores while the agent scans with the template's
        // `ignoreExtra`, so without this a path present in both trees is
        // absent from `remote_files`, lands in the `None` arm below, uploads,
        // is excluded from the next scan, and is requested again — every run,
        // forever.
        if fileset::would_ignore(workspace_root, rel_path, false, extra_ignores) {
            continue;
        }

        match remote_files.get(*rel_path) {
            Some(remote_meta) => {
                if remote_meta.hash != client_entry.hash {
                    want.push((*rel_path).to_string());
                }
            }
            // Not on the agent yet, and not ignored: genuinely new.
            None => {
                want.push((*rel_path).to_string());
            }
        }
    }

    let mut delete_extraneous = Vec::new();
    for rel_path in remote_files.keys() {
        if !client_map.contains_key(rel_path.as_str()) {
            // Section 5.1 Deletion Safety Rule:
            // Never delete files located in default ignored directories (node_modules, target, etc.)
            // and never delete agent-internal state or history files (.farhand-state.json, .farhand-runs).
            if !fileset::is_default_ignored(rel_path)
                && rel_path != state::STATE_FILENAME
                && !rel_path.starts_with(".farhand-")
            {
                delete_extraneous.push(rel_path.clone());
            }
        }
    }

    want.sort();
    delete_extraneous.sort();

    Ok(DiffResult {
        want,
        delete_extraneous,
    })
}

/// Total on-disk size of the paths an artifact transfer would include.
///
/// Used to refuse an oversized artifact *before* anything is packed, since the
/// archive is built in memory. Symlinks are counted as links, never followed,
/// so a link cannot inflate the total.
pub fn sum_artifact_bytes(workspace_root: &Path, rel_paths: &[String]) -> u64 {
    let mut total = 0u64;
    for rel in rel_paths {
        let Ok(rel_buf) = protocol::from_wire_path(rel) else {
            continue;
        };
        let path = workspace_root.join(rel_buf);
        match std::fs::symlink_metadata(&path) {
            Ok(m) if m.is_dir() => total = total.saturating_add(calculate_dir_size(&path)),
            Ok(m) => total = total.saturating_add(m.len()),
            Err(_) => continue,
        }
    }
    total
}

/// Safely remove files listed in `to_delete` from `workspace_root` and prune empty parent folders.
pub fn apply_deletions(workspace_root: &Path, to_delete: &[String]) -> std::io::Result<usize> {
    if !workspace_root.exists() {
        return Ok(0);
    }

    let canonical_root = workspace_root.canonicalize()?;
    let mut count = 0;

    for rel_path in to_delete {
        let safe_rel = match protocol::from_wire_path(rel_path) {
            Ok(p) => p,
            Err(_) => continue,
        };

        let target = canonical_root.join(&safe_rel);
        if target.exists() {
            if let Ok(canonical_target) = target.canonicalize() {
                if canonical_target.starts_with(&canonical_root)
                    && canonical_target != canonical_root
                    && fs::remove_file(&canonical_target).is_ok()
                {
                    count += 1;
                    // Prune empty parent directories up to workspace root
                    let mut parent = canonical_target.parent();
                    while let Some(p) = parent {
                        if p == canonical_root {
                            break;
                        }
                        if fs::remove_dir(p).is_err() {
                            break; // Directory not empty
                        }
                        parent = p.parent();
                    }
                }
            }
        }
    }

    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_resolve_workspace_dir() {
        let base = Path::new("/var/farhand/workspaces");
        let dir1 = resolve_workspace_dir(base, "my-app");
        let dir2 = resolve_workspace_dir(base, "my-app");
        assert_eq!(dir1, dir2);
        assert_eq!(dir1.parent(), Some(base));
        assert!(dir1
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|s| s.starts_with("my-app-")));

        let dir3 = resolve_workspace_dir(base, "other-project");
        assert_ne!(dir1, dir3);
    }

    #[test]
    fn test_diff_fresh_workspace() {
        let dir = tempdir().unwrap();
        let ws = dir.path().join("ws");

        let manifest = ManifestPayload {
            files: vec![
                FileEntry {
                    path: "src/main.rs".into(),
                    hash: "hash1".into(),
                    size: 10,
                    mode: 0o644,
                },
                FileEntry {
                    path: "Cargo.toml".into(),
                    hash: "hash2".into(),
                    size: 20,
                    mode: 0o644,
                },
            ],
        };

        let diff = diff_manifests(&ws, &manifest, &[]).unwrap();
        assert_eq!(diff.want, vec!["Cargo.toml", "src/main.rs"]);
        assert!(diff.delete_extraneous.is_empty());
    }

    #[test]
    fn test_diff_unchanged_workspace() {
        let dir = tempdir().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(ws.join("src")).unwrap();

        fs::write(ws.join("src/main.rs"), b"fn main() {}").unwrap();
        let hash = fileset::hash_file(&ws.join("src/main.rs")).unwrap();

        let manifest = ManifestPayload {
            files: vec![FileEntry {
                path: "src/main.rs".into(),
                hash,
                size: 12,
                mode: 0o644,
            }],
        };

        let diff = diff_manifests(&ws, &manifest, &[]).unwrap();
        assert!(
            diff.want.is_empty(),
            "Unchanged files should not be in want: {:?}",
            diff.want
        );
        assert!(diff.delete_extraneous.is_empty());
    }

    #[test]
    fn test_diff_modified_file() {
        let dir = tempdir().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();

        fs::write(ws.join("file.txt"), b"remote content").unwrap();

        let manifest = ManifestPayload {
            files: vec![FileEntry {
                path: "file.txt".into(),
                hash: "different_local_hash".into(),
                size: 20,
                mode: 0o644,
            }],
        };

        let diff = diff_manifests(&ws, &manifest, &[]).unwrap();
        assert_eq!(diff.want, vec!["file.txt"]);
        assert!(diff.delete_extraneous.is_empty());
    }

    #[test]
    fn test_deletion_safety_rule_section_5_1() {
        let dir = tempdir().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(ws.join("src")).unwrap();
        fs::create_dir_all(ws.join("node_modules/react")).unwrap();
        fs::create_dir_all(ws.join("target/release")).unwrap();

        // 1. Legitimate source file that exists remotely
        fs::write(ws.join("src/old_deleted_file.rs"), b"old").unwrap();

        // 2. Remote cached dependencies (must NOT be deleted)
        fs::write(ws.join("node_modules/react/index.js"), b"react").unwrap();
        fs::write(ws.join("target/release/binary"), b"elf").unwrap();

        // Client manifest only tracks new_file.rs (old_deleted_file.rs was removed locally)
        let manifest = ManifestPayload {
            files: vec![FileEntry {
                path: "src/new_file.rs".into(),
                hash: "new_hash".into(),
                size: 10,
                mode: 0o644,
            }],
        };

        let diff = diff_manifests(&ws, &manifest, &[]).unwrap();

        // old_deleted_file.rs should be flagged for deletion
        assert_eq!(diff.delete_extraneous, vec!["src/old_deleted_file.rs"]);

        // CRITICAL: node_modules and target files MUST NOT be in delete_extraneous
        assert!(!diff
            .delete_extraneous
            .iter()
            .any(|p| p.contains("node_modules")));
        assert!(!diff.delete_extraneous.iter().any(|p| p.contains("target")));

        // Test apply_deletions
        let deleted = apply_deletions(&ws, &diff.delete_extraneous).unwrap();
        assert_eq!(deleted, 1);
        assert!(!ws.join("src/old_deleted_file.rs").exists());

        // Verify node_modules and target survived untouched
        assert!(ws.join("node_modules/react/index.js").exists());
        assert!(ws.join("target/release/binary").exists());
    }

    /// The digest index lives inside the scanned root, so it is the one file
    /// most likely to be mistaken for project state. It must never be
    /// transferred, never be deleted, and must not perturb the diff.
    #[test]
    fn test_digest_index_is_never_transferred_or_deleted() {
        let dir = tempdir().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();
        fs::write(ws.join("file.txt"), b"content").unwrap();

        let manifest = ManifestPayload {
            files: vec![FileEntry {
                path: "file.txt".into(),
                hash: fileset::hash_file(&ws.join("file.txt")).unwrap(),
                size: 7,
                mode: 0o644,
            }],
        };

        let first = diff_manifests(&ws, &manifest, &[]).unwrap();
        assert!(first.want.is_empty());
        assert!(first.delete_extraneous.is_empty());

        let index = ws.join(fileset::HASH_INDEX_FILENAME);
        assert!(index.is_file(), "the diff did not persist a digest index");

        // A second diff, driven entirely by the on-disk index, must agree.
        let second = diff_manifests(&ws, &manifest, &[]).unwrap();
        assert!(second.want.is_empty());
        assert!(
            second.delete_extraneous.is_empty(),
            "the index was treated as project state: {:?}",
            second.delete_extraneous
        );

        // And it must survive the deletions the diff asked for.
        fs::write(ws.join("stray.txt"), b"stray").unwrap();
        let third = diff_manifests(&ws, &manifest, &[]).unwrap();
        assert_eq!(third.delete_extraneous, vec!["stray.txt"]);
        apply_deletions(&ws, &third.delete_extraneous).unwrap();
        assert!(index.is_file(), "the digest index was deleted");
    }

    /// A file edited on the agent between two diffs must be re-detected even
    /// though the second diff reads its digest from the index rather than
    /// hashing the file again.
    #[test]
    fn test_digest_index_does_not_mask_a_remote_edit() {
        let dir = tempdir().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();
        fs::write(ws.join("file.txt"), b"first").unwrap();

        let stale = ManifestPayload {
            files: vec![FileEntry {
                path: "file.txt".into(),
                hash: fileset::hash_file(&ws.join("file.txt")).unwrap(),
                size: 5,
                mode: 0o644,
            }],
        };

        // Populate the index.
        assert!(diff_manifests(&ws, &stale, &[]).unwrap().want.is_empty());

        // The agent rewrites the file. Same length, so only a content change
        // distinguishes it.
        fs::write(ws.join("file.txt"), b"other").unwrap();

        let diff = diff_manifests(&ws, &stale, &[]).unwrap();
        assert_eq!(
            diff.want,
            vec!["file.txt"],
            "the cached digest masked a remote edit"
        );
    }

    /// A path the template ignores on the agent must never be requested, no
    /// matter how many times the same manifest is replayed.
    ///
    /// The client scans with no extra ignores and the agent scans with the
    /// template's `ignoreExtra`, so before this was fixed the file was absent
    /// from `remote_files`, fell into the "not present" arm, was uploaded,
    /// stayed excluded from the next scan, and was uploaded again on the next
    /// run — a permanent re-upload that showed up as a sync which never
    /// reaches zero bytes.
    #[test]
    fn test_ignored_path_is_never_requested_however_often_the_manifest_replays() {
        let dir = tempdir().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();

        // The file exists on the client and is in its manifest.
        let client_side = dir.path().join("client");
        fs::create_dir_all(&client_side).unwrap();
        fs::write(client_side.join("build.log"), b"chatter").unwrap();

        let manifest = ManifestPayload {
            files: vec![FileEntry {
                path: "build.log".into(),
                hash: fileset::hash_file(&client_side.join("build.log")).unwrap(),
                size: 7,
                mode: 0o644,
            }],
        };

        let ignored = vec!["*.log".to_string()];

        // First run: the file is not on the agent and must not be requested.
        let first = diff_manifests(&ws, &manifest, &ignored).unwrap();
        assert!(
            !first.want.contains(&"build.log".to_string()),
            "an ignored path was requested: {:?}",
            first.want
        );

        // Second run: nothing was uploaded, so the tree is unchanged and the
        // answer must be identical. A regression here is what turned into an
        // upload-forever loop.
        let second = diff_manifests(&ws, &manifest, &ignored).unwrap();
        assert_eq!(first.want, second.want, "the want set is not stable");
        assert!(!second.want.contains(&"build.log".to_string()));
    }

    /// The negative control: a file the template does *not* ignore is still
    /// transferred, so the fix cannot be satisfied by requesting nothing.
    #[test]
    fn test_unignored_new_file_is_still_requested() {
        let dir = tempdir().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();

        let client_side = dir.path().join("client");
        fs::create_dir_all(&client_side).unwrap();
        fs::write(client_side.join("main.rs"), b"fn main() {}").unwrap();

        let manifest = ManifestPayload {
            files: vec![FileEntry {
                path: "main.rs".into(),
                hash: fileset::hash_file(&client_side.join("main.rs")).unwrap(),
                size: 12,
                mode: 0o644,
            }],
        };

        let diff = diff_manifests(&ws, &manifest, &["*.log".to_string()]).unwrap();
        assert_eq!(diff.want, vec!["main.rs"]);
    }

    /// The blind spot a `(size, mtime)` gate cannot see, exercised through the
    /// real platform change token and the real `diff_manifests` path. A build
    /// tool that silently misses this ships stale code, so the guarantee is
    /// worth pinning.
    #[test]
    fn test_diff_detects_a_same_size_overwrite_that_preserves_the_mtime() {
        let dir = tempdir().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();
        let path = ws.join("file.txt");
        fs::write(&path, b"aaaa").unwrap();

        let original_mtime = fs::metadata(&path).unwrap().modified().unwrap();

        // The client's copy: same bytes the agent already has.
        let manifest = ManifestPayload {
            files: vec![FileEntry {
                path: "file.txt".into(),
                hash: fileset::hash_file(&path).unwrap(),
                size: 4,
                mode: 0o644,
            }],
        };
        assert!(
            diff_manifests(&ws, &manifest, &[]).unwrap().want.is_empty(),
            "the first diff should see no change"
        );

        // Something on the agent rewrote the file to different content of the
        // same length, then restored the original mtime — what an `rsync -a`
        // or `tar -p` extraction onto the workspace does.
        fs::write(&path, b"bbbb").unwrap();
        restore_mtime(&path, original_mtime);

        let diff = diff_manifests(&ws, &manifest, &[]).unwrap();
        assert_eq!(
            diff.want,
            vec!["file.txt"],
            "a preserved-mtime same-size rewrite was masked by the digest cache"
        );
    }

    #[cfg(unix)]
    fn restore_mtime(path: &Path, mtime: std::time::SystemTime) {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(path).unwrap().permissions().mode();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
    }

    #[cfg(windows)]
    fn restore_mtime(path: &Path, mtime: std::time::SystemTime) {
        // Windows does let userspace forge LastWriteTime, which is exactly why
        // the digest gate reads the change time separately.
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
    }
}
