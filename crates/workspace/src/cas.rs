use crate::cow::cow_clone_file;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;
use tracing::debug;

/// Process-local counter ensuring unique tmp names for concurrent stores.
static CAS_TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

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

        // Unique tmp name per call: two concurrent stores of the same hash must
        // not race on a shared tmp path (a process-constant tmp.{pid} used to
        // collide).
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let counter = CAS_TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp_name = format!(
            "{}.tmp.{}.{}.{}",
            sha256,
            std::process::id(),
            nanos,
            counter
        );
        let tmp_path = dst_path.with_file_name(tmp_name);
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

    /// Materialize an object from CAS into `dest_path` via CoW reflink or copy.
    ///
    /// Returns `Ok(true)` if materialized, or `Ok(false)` if the object is
    /// missing from CAS. A successful hydration **touches the object's mtime**
    /// so CAS GC evicts by "unused since" age rather than ingestion age.
    pub fn materialize_to(&self, sha256: &str, dest_path: &Path) -> io::Result<bool> {
        let cas_path = self.object_path(sha256);
        if !cas_path.is_file() {
            return Ok(false);
        }

        if let Some(parent) = dest_path.parent() {
            fs::create_dir_all(parent)?;
        }

        cow_clone_file(&cas_path, dest_path)?;

        // Touch: keep LRU accounting accurate for GC. Best-effort — a failed
        // touch must not fail hydration.
        if let Ok(f) = fs::File::options().write(true).open(&cas_path) {
            let _ = f.set_modified(SystemTime::now());
        }

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

#[cfg(test)]
mod concurrency_tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::tempdir;

    #[test]
    fn test_concurrent_put_file_same_hash_never_clobbers() {
        // Regression: the old process-constant tmp.{pid} name made two
        // concurrent stores of the same hash race on one tmp file.
        let workdir = tempdir().unwrap();
        let cas = Arc::new(CasStore::new(workdir.path()));
        let src = tempdir().unwrap();
        let src_file = src.path().join("data.bin");
        fs::write(&src_file, vec![42u8; 8192]).unwrap();
        let hash = "abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234";

        let mut handles = Vec::new();
        for _ in 0..32 {
            let cas = Arc::clone(&cas);
            let src_file = src_file.clone();
            let hash = hash.to_string();
            handles.push(std::thread::spawn(move || {
                cas.put_file(&hash, &src_file).expect("concurrent put_file")
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let stored = fs::read(cas.object_path(hash)).unwrap();
        assert_eq!(
            stored,
            vec![42u8; 8192],
            "object must be intact after the race"
        );
    }

    #[test]
    fn test_materialize_touches_object_mtime() {
        let workdir = tempdir().unwrap();
        let cas = CasStore::new(workdir.path());
        let src = tempdir().unwrap();
        let src_file = src.path().join("f.bin");
        fs::write(&src_file, b"touch-me").unwrap();
        let hash = "abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234";
        cas.put_file(hash, &src_file).unwrap();

        let before = fs::metadata(cas.object_path(hash))
            .unwrap()
            .modified()
            .unwrap();
        // Age the object artificially so a touch is observable.
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(600);
        {
            let f = fs::File::options()
                .write(true)
                .open(cas.object_path(hash))
                .unwrap();
            f.set_modified(old).unwrap();
        }

        let dest = tempdir().unwrap();
        cas.materialize_to(hash, &dest.path().join("out.bin"))
            .unwrap();

        let after = fs::metadata(cas.object_path(hash))
            .unwrap()
            .modified()
            .unwrap();
        assert!(
            after > old,
            "materialize_to must touch the object mtime for LRU accounting"
        );
        let _ = before;
    }
}
