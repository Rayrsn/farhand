use fileset::FilesetError;
use protocol::{FileEntry, ManifestPayload};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

pub mod presets;
pub use presets::{detect_preset_outputs, resolve_artifact_paths, Preset, DEFAULT_PRESETS};

pub mod lock;
pub use lock::WorkspaceLockManager;

pub mod state;
pub use state::{compute_lockfiles_hash, read_state, write_state, WorkspaceState};

pub mod history;
pub use history::{format_rfc3339, get_recent_runs, save_run, MAX_RUNS_RETAINED, RUNS_DIR};

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

    let remote_files = if workspace_root.exists() {
        fileset::scan(workspace_root, extra_ignores)?
    } else {
        HashMap::new()
    };

    let mut want = Vec::new();
    for (rel_path, client_entry) in &client_map {
        match remote_files.get(*rel_path) {
            Some(remote_meta) => {
                if remote_meta.hash != client_entry.hash {
                    want.push((*rel_path).to_string());
                }
            }
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
                && !rel_path.starts_with(".farhand-runs")
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
        assert!(dir1
            .to_string_lossy()
            .starts_with("/var/farhand/workspaces/my-app-"));

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
}
