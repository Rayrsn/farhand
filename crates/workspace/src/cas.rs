use crate::cow::cow_clone_file;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use tracing::debug;

/// Global Content-Addressable Storage (CAS) for cross-project and cross-branch deduplication.
///
/// Files are indexed by their hex-encoded SHA-256 digests. Workspaces materialize
/// files from CAS using native APFS / Linux CoW reflinks or hardlinks, saving disk space
/// and bypassing network uploads completely when files are already known.
#[derive(Debug, Clone)]
pub struct CasStore {
    objects_dir: PathBuf,
}

impl CasStore {
    /// Initialize a CAS store within `base_workdir/cas/objects`.
    pub fn new(base_workdir: &Path) -> Self {
        let objects_dir = base_workdir.join("cas").join("objects");
        let _ = fs::create_dir_all(&objects_dir);
        Self { objects_dir }
    }

    pub fn objects_dir(&self) -> &Path {
        &self.objects_dir
    }

    /// Resolve on-disk storage path for an object: `cas/objects/ab/cd/abcdef...`
    pub fn object_path(&self, sha256: &str) -> PathBuf {
        if sha256.len() >= 4 {
            self.objects_dir
                .join(&sha256[..2])
                .join(&sha256[2..4])
                .join(sha256)
        } else {
            self.objects_dir.join(sha256)
        }
    }

    /// Check whether an object with the given SHA-256 hash exists in CAS.
    pub fn has_object(&self, sha256: &str) -> bool {
        self.object_path(sha256).is_file()
    }

    /// Store a file in CAS under its SHA-256 hash.
    /// Uses atomic rename to prevent half-written objects under concurrent stores.
    pub fn put_file(&self, sha256: &str, src_path: &Path) -> io::Result<PathBuf> {
        if !src_path.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("Source file does not exist: {}", src_path.display()),
            ));
        }

        let dst_path = self.object_path(sha256);
        if dst_path.is_file() {
            return Ok(dst_path);
        }

        if let Some(parent) = dst_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let tmp_path = dst_path.with_extension(format!("tmp.{}", std::process::id()));
        cow_clone_file(src_path, &tmp_path)?;
        if let Err(e) = fs::rename(&tmp_path, &dst_path) {
            let _ = fs::remove_file(&tmp_path);
            if !dst_path.is_file() {
                return Err(e);
            }
        }

        debug!(
            "Registered object in CAS: {} ({})",
            sha256,
            src_path.display()
        );
        Ok(dst_path)
    }

    /// Materialize an object from CAS into `dest_path` via CoW reflink or hardlink.
    /// Returns `Ok(true)` if materialized, or `Ok(false)` if the object is missing from CAS.
    pub fn materialize_to(&self, sha256: &str, dest_path: &Path) -> io::Result<bool> {
        let cas_path = self.object_path(sha256);
        if !cas_path.is_file() {
            return Ok(false);
        }

        if let Some(parent) = dest_path.parent() {
            fs::create_dir_all(parent)?;
        }

        cow_clone_file(&cas_path, dest_path)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_cas_store_put_and_materialize() {
        let workdir = tempdir().unwrap();
        let cas = CasStore::new(workdir.path());

        let src_dir = tempdir().unwrap();
        let file_path = src_dir.path().join("code.rs");
        fs::write(&file_path, b"fn main() { println!(\"cas\"); }").unwrap();

        let hash = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

        assert!(!cas.has_object(hash));

        cas.put_file(hash, &file_path).unwrap();
        assert!(cas.has_object(hash));

        let dest_dir = tempdir().unwrap();
        let target_file = dest_dir.path().join("sub/hydrated.rs");
        let ok = cas.materialize_to(hash, &target_file).unwrap();
        assert!(ok);
        assert_eq!(
            fs::read(&target_file).unwrap(),
            b"fn main() { println!(\"cas\"); }"
        );

        // Missing object returns false
        assert!(!cas
            .materialize_to("nonexistenthash", &dest_dir.path().join("missing.rs"))
            .unwrap());
    }
}
