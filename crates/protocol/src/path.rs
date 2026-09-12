use std::path::{Path, PathBuf};

/// Converts a local filesystem relative path into a wire-normalized forward-slash path.
///
/// Strips leading and trailing slashes and replaces any backslashes with `/`.
pub fn to_wire_path(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    s.trim_matches('/').to_string()
}

/// Validates and converts an incoming wire-path string into a host-native relative `PathBuf`.
///
/// Invariant Security Rules (AGENTS.md §3.1 & §3.2):
/// - Rejects empty paths.
/// - Rejects absolute paths (starting with `/` or Windows drive specifiers `C:`).
/// - Rejects backslashes (`\`) on ingest.
/// - Rejects directory traversal sequences (`..`).
pub fn from_wire_path(wire: &str) -> Result<PathBuf, String> {
    let clean = wire.trim();
    if clean.is_empty() {
        return Err("empty wire path".to_string());
    }

    if clean.starts_with('/') {
        return Err(format!("insecure absolute path in wire protocol: {}", wire));
    }

    if clean.contains('\\') {
        return Err(format!("invalid backslash in wire protocol path: {}", wire));
    }

    // Reject Windows drive specifiers e.g. "C:foo" or "D:/bar"
    let bytes = clean.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Err(format!(
            "insecure drive specifier in wire protocol: {}",
            wire
        ));
    }

    // Reject path traversal components
    for part in clean.split('/') {
        if part == ".." {
            return Err(format!(
                "insecure path traversal '..' in wire protocol: {}",
                wire
            ));
        }
    }

    Ok(PathBuf::from(clean))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_wire_path() {
        assert_eq!(to_wire_path(Path::new("src/main.rs")), "src/main.rs");
        assert_eq!(to_wire_path(Path::new("src\\main.rs")), "src/main.rs");
        assert_eq!(
            to_wire_path(Path::new("\\nested\\dir\\file.txt")),
            "nested/dir/file.txt"
        );
        assert_eq!(to_wire_path(Path::new("/unix/path/")), "unix/path");
    }

    #[test]
    fn test_from_wire_path_valid() {
        let p = from_wire_path("crates/protocol/src/lib.rs").unwrap();
        assert_eq!(p, PathBuf::from("crates/protocol/src/lib.rs"));

        let p2 = from_wire_path("README.md").unwrap();
        assert_eq!(p2, PathBuf::from("README.md"));
    }

    #[test]
    fn test_from_wire_path_reject_traversal() {
        assert!(from_wire_path("../secret").is_err());
        assert!(from_wire_path("foo/../bar").is_err());
        assert!(from_wire_path("foo/bar/..").is_err());
    }

    #[test]
    fn test_from_wire_path_reject_absolute_and_drives() {
        assert!(from_wire_path("/etc/passwd").is_err());
        assert!(from_wire_path("/root/.ssh/id_rsa").is_err());
        assert!(from_wire_path("C:/Windows/System32").is_err());
        assert!(from_wire_path("D:evil").is_err());
    }

    #[test]
    fn test_from_wire_path_reject_backslashes() {
        assert!(from_wire_path("foo\\bar").is_err());
        assert!(from_wire_path("src\\lib.rs").is_err());
    }

    #[test]
    fn test_from_wire_path_reject_empty() {
        assert!(from_wire_path("").is_err());
        assert!(from_wire_path("   ").is_err());
    }
}
