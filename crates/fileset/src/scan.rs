use crate::ignore::IgnoreMatcher;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use thiserror::Error;
use walkdir::WalkDir;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMeta {
    /// Slash-separated relative path
    pub path: String,
    /// Hex SHA-256 digest
    pub hash: String,
    /// File size in bytes
    pub size: u64,
    /// POSIX file permission bits
    pub mode: u32,
    /// Modified time in nanoseconds since Unix epoch
    pub modified_nanos: i64,
}

#[derive(Error, Debug)]
pub enum FilesetError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("WalkDir error: {0}")]
    WalkDir(#[from] walkdir::Error),

    #[error("Insecure path in archive: {0}")]
    InsecurePath(String),

    #[error("Archive path escapes target root: {0}")]
    EscapesTargetRoot(String),

    #[error("Tar error: {0}")]
    Tar(String),
}

/// Compute streaming SHA-256 digest of a file.
pub fn hash_file(path: &Path) -> std::io::Result<String> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];

    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }

    Ok(hex::encode(hasher.finalize()))
}

/// Retrieve file mode permission bits (cross-platform).
pub fn get_file_mode(metadata: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode()
    }
    #[cfg(not(unix))]
    {
        if metadata.permissions().readonly() {
            0o444
        } else {
            0o644
        }
    }
}

/// Scan a directory, applying default ignore rules, .gitignore, .farhand-ignore,
/// and extra caller patterns. Returns a map of relative paths to FileMeta.
pub fn scan(
    root: &Path,
    extra_ignores: &[String],
) -> Result<HashMap<String, FileMeta>, FilesetError> {
    let mut matcher = IgnoreMatcher::new();
    matcher.load_file(&root.join(".gitignore"));
    matcher.load_file(&root.join(".farhand-ignore"));
    matcher.add_patterns(extra_ignores);

    let mut result = HashMap::new();
    let mut walker = WalkDir::new(root).follow_links(false).into_iter();

    while let Some(entry_res) = walker.next() {
        let entry = entry_res?;
        let entry_path = entry.path();

        if entry_path == root {
            continue;
        }

        let rel = match entry_path.strip_prefix(root) {
            Ok(r) => r,
            Err(_) => continue,
        };

        let rel_wire_path = protocol::to_wire_path(rel);
        let is_dir = entry.file_type().is_dir();

        if is_dir {
            if matcher.should_ignore(&rel_wire_path, true) {
                walker.skip_current_dir();
                continue;
            }
        } else if entry.file_type().is_file() {
            if matcher.should_ignore(&rel_wire_path, false) {
                continue;
            }

            let metadata = entry.metadata()?;
            let hash = hash_file(entry_path)?;
            let size = metadata.len();
            let mode = get_file_mode(&metadata);

            let modified_nanos = metadata
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as i64)
                .unwrap_or(0);

            result.insert(
                rel_wire_path.clone(),
                FileMeta {
                    path: rel_wire_path,
                    hash,
                    size,
                    mode,
                    modified_nanos,
                },
            );
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_scan_directory_and_ignores() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        // Create directory tree
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("node_modules/fake-pkg")).unwrap();
        fs::create_dir_all(root.join("target/debug")).unwrap();

        fs::write(root.join("src/main.rs"), b"fn main() {}").unwrap();
        fs::write(root.join("src/lib.rs"), b"pub fn add() {}").unwrap();
        fs::write(
            root.join("node_modules/fake-pkg/index.js"),
            b"module.exports = 1;",
        )
        .unwrap();
        fs::write(root.join("target/debug/binary"), b"ELF_DATA").unwrap();
        fs::write(root.join("temp.log"), b"log data").unwrap();

        // Add .gitignore
        fs::write(root.join(".gitignore"), b"*.log\n").unwrap();

        let scanned = scan(root, &[]).unwrap();

        // Must include tracked source files
        assert!(scanned.contains_key("src/main.rs"));
        assert!(scanned.contains_key("src/lib.rs"));
        assert!(scanned.contains_key(".gitignore"));

        // Must NOT include default ignored directories
        assert!(!scanned.contains_key("node_modules/fake-pkg/index.js"));
        assert!(!scanned.contains_key("target/debug/binary"));

        // Must NOT include .gitignore matches
        assert!(!scanned.contains_key("temp.log"));

        let main_meta = scanned.get("src/main.rs").unwrap();
        assert_eq!(main_meta.path, "src/main.rs");
        assert_eq!(main_meta.size, 12);
        assert!(!main_meta.hash.is_empty());
    }
}
