use glob::glob;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;
use std::time::SystemTime;

pub const STATE_FILENAME: &str = ".farhand-state.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceState {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(rename = "lastSuccessLockfileHash")]
    pub last_success_lockfile_hash: String,
    #[serde(rename = "lastInstalledAt")]
    pub last_installed_at: SystemTime,
    pub template: String,
}

fn default_version() -> u32 {
    1
}

/// Computes a composite SHA-256 hash of all files matching the declared lockfile patterns.
///
/// Matches patterns relative to `workspace_root` (e.g. `Cargo.lock`, `package-lock.json`, `**/package-lock.json`).
/// Normalizes paths to wire paths to ensure identical hashes across platforms.
pub fn compute_lockfiles_hash(workspace_root: &Path, lockfiles: &[String]) -> Option<String> {
    let mut entries = Vec::new();

    for pattern in lockfiles {
        let pattern_clean = pattern.trim_start_matches('/');
        let full_pattern = workspace_root.join(pattern_clean);
        if let Ok(paths) = glob(&full_pattern.to_string_lossy()) {
            for entry in paths.flatten() {
                if entry.is_file() {
                    if let Ok(bytes) = fs::read(&entry) {
                        let mut file_hasher = Sha256::new();
                        file_hasher.update(&bytes);
                        let file_hash = hex::encode(file_hasher.finalize());
                        let rel = entry.strip_prefix(workspace_root).unwrap_or(&entry);
                        let wire_path = protocol::to_wire_path(rel);
                        entries.push(format!("{}:{}", wire_path, file_hash));
                    }
                }
            }
        }
    }

    if entries.is_empty() {
        return None;
    }

    entries.sort();
    let mut combined_hasher = Sha256::new();
    combined_hasher.update(entries.join("|").as_bytes());
    Some(hex::encode(combined_hasher.finalize()))
}

/// Reads `.farhand-state.json` from `workspace_dir` if present and valid.
pub fn read_state(workspace_dir: &Path) -> Option<WorkspaceState> {
    let state_file = workspace_dir.join(STATE_FILENAME);
    let content = fs::read_to_string(state_file).ok()?;
    serde_json::from_str(&content).ok()
}

/// Writes `WorkspaceState` to `.farhand-state.json` in `workspace_dir`.
pub fn write_state(workspace_dir: &Path, state: &WorkspaceState) -> std::io::Result<()> {
    let state_file = workspace_dir.join(STATE_FILENAME);
    let json = serde_json::to_string_pretty(state)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    fs::write(state_file, json)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_compute_lockfiles_hash_single_and_multiple() {
        let dir = tempdir().unwrap();
        let cargo_lock = dir.path().join("Cargo.lock");
        let pnpm_lock = dir.path().join("pnpm-lock.yaml");

        fs::write(&cargo_lock, "version = 3\n").unwrap();
        fs::write(&pnpm_lock, "lockfileVersion: 5.4\n").unwrap();

        let hash_single = compute_lockfiles_hash(dir.path(), &["Cargo.lock".to_string()]);
        assert!(hash_single.is_some());

        let hash_both1 = compute_lockfiles_hash(
            dir.path(),
            &["Cargo.lock".to_string(), "pnpm-lock.yaml".to_string()],
        );
        let hash_both2 = compute_lockfiles_hash(
            dir.path(),
            &["pnpm-lock.yaml".to_string(), "Cargo.lock".to_string()],
        );
        assert_eq!(
            hash_both1, hash_both2,
            "sort order must produce identical composite hash"
        );
        assert_ne!(hash_single, hash_both1);
    }

    #[test]
    fn test_compute_lockfiles_hash_nonexistent() {
        let dir = tempdir().unwrap();
        let hash = compute_lockfiles_hash(dir.path(), &["Cargo.lock".to_string()]);
        assert!(hash.is_none());
    }

    #[test]
    fn test_state_read_write_roundtrip() {
        let dir = tempdir().unwrap();
        assert!(read_state(dir.path()).is_none());

        let state = WorkspaceState {
            version: 1,
            last_success_lockfile_hash: "abcd1234ef".to_string(),
            last_installed_at: SystemTime::now(),
            template: "npm".to_string(),
        };

        write_state(dir.path(), &state).unwrap();
        let loaded = read_state(dir.path()).unwrap();
        assert_eq!(loaded.version, state.version);
        assert_eq!(
            loaded.last_success_lockfile_hash,
            state.last_success_lockfile_hash
        );
        assert_eq!(loaded.template, state.template);
    }
}
