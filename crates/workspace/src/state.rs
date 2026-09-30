use glob::glob;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
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
    /// The project name the client used, e.g. `my-repo:feat/payments`.
    ///
    /// The workspace *directory* name is sanitised and hash-suffixed
    /// (`my-repo__feat-payments-1a2b3c4d`), so the original cannot be
    /// recovered from it. Locks are keyed by this raw name, so GC and CLEAN
    /// have to read it back from here or their lock check can never match.
    #[serde(default)]
    pub project: String,
    /// Toolchain pins in force when the last successful install ran.
    ///
    /// Compared against the run's toolchain on the next build, so pinning
    /// `-T node=22` re-runs the dependency hook instead of trusting a
    /// `node_modules` built under 18.
    #[serde(default)]
    pub toolchain: BTreeMap<String, String>,
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
        // The *pattern* needs normalising, not just the result. On Windows a
        // `Path` renders with backslashes and `\` is glob's escape character,
        // so the match found nothing and the hash came back empty — which
        // made the install hook believe dependencies had never been fetched.
        //
        // Note this does *not* use `protocol::to_wire_path`, which is for
        // relative wire paths: it trims the leading `/`, turning an absolute
        // glob into a relative one that matches nothing on every platform.
        let pattern_rel = pattern.trim_start_matches('/').replace('\\', "/");
        let root_str = workspace_root.to_string_lossy().replace('\\', "/");
        let pattern_str = format!("{}/{}", root_str.trim_end_matches('/'), pattern_rel);
        if let Ok(paths) = glob(&pattern_str) {
            for entry in paths.flatten() {
                if entry.is_file() {
                    if let Ok(bytes) = fs::read(&entry) {
                        let mut file_hasher = Sha256::new();
                        file_hasher.update(&bytes);
                        let file_hash = hex::encode(file_hasher.finalize());
                        let rel = entry.strip_prefix(workspace_root).unwrap_or(&entry);
                        let wire_path = protocol::to_wire_path(rel);
                        entries.push(format!("{wire_path}:{file_hash}"));
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
            project: "my-repo:feat/payments".to_string(),
            toolchain: std::collections::BTreeMap::new(),
        };

        write_state(dir.path(), &state).unwrap();
        let loaded = read_state(dir.path()).unwrap();
        assert_eq!(loaded.version, state.version);
        assert_eq!(
            loaded.last_success_lockfile_hash,
            state.last_success_lockfile_hash
        );
        assert_eq!(loaded.template, state.template);
        assert_eq!(loaded.project, state.project);
    }
}

#[cfg(test)]
mod lockfile_pattern_tests {
    use super::*;
    use tempfile::tempdir;

    /// The *pattern* needs normalising, not just the result: on Windows
    /// `Path` renders as `C:\ws\Cargo.lock` and `\` is glob's escape
    /// character, so the match found nothing and the hash came back empty.
    /// That made the install hook believe dependencies had never been fetched.
    #[test]
    fn a_plain_lockfile_is_found_and_hashed() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("deps.lock"), "dep-version-1\n").unwrap();

        let first = compute_lockfiles_hash(dir.path(), &["deps.lock".to_string()]);
        assert!(first.is_some(), "a declared lockfile was not found");

        // Changing it must change the hash, or the install hook would never
        // re-run when dependencies actually move.
        fs::write(dir.path().join("deps.lock"), "dep-version-2\n").unwrap();
        let second = compute_lockfiles_hash(dir.path(), &["deps.lock".to_string()]);
        assert!(second.is_some());
        assert_ne!(first, second, "the hash did not track the file's content");
    }

    #[test]
    fn a_leading_slash_pattern_still_resolves() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.lock"), "x\n").unwrap();
        assert!(compute_lockfiles_hash(dir.path(), &["/Cargo.lock".to_string()]).is_some());
    }
}
