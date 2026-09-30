use crate::ignore::{is_default_ignored, IgnoreMatcher, DEFAULT_IGNORES};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
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
/// Build the ignore matcher exactly as [`scan`] does.
///
/// Shared so that an explanation of *why* a path was skipped can never drift
/// from the rules the scan actually applied — a `why` built from different
/// rules would confidently report the wrong reason.
fn build_matcher(root: &Path, extra_ignores: &[String]) -> IgnoreMatcher {
    let mut matcher = IgnoreMatcher::new();
    matcher.load_file(&root.join(".gitignore"));
    matcher.load_file(&root.join(".farhand-ignore"));
    matcher.add_patterns(extra_ignores);
    matcher
}

/// Explain why `rel_path` is excluded from a sync, if it is.
///
/// Uses the same matcher as [`scan`], so the answer is the real one. Returns
/// `None` when the path is not ignored, which means the caller should look at
/// the agent's state instead (already present, or due to transfer).
pub fn explain_ignore(root: &Path, rel_path: &str, extra_ignores: &[String]) -> Option<String> {
    if is_default_ignored(rel_path) {
        let component = rel_path
            .split('/')
            .find(|part| !part.is_empty() && DEFAULT_IGNORES.contains(part))?;
        return Some(format!("built-in ignore: {component}/"));
    }
    let matcher = build_matcher(root, extra_ignores);
    matcher
        .matching_rule(rel_path, false)
        .map(|pattern| format!("ignore rule: {pattern}"))
}

/// Whether a path is excluded by the same rules [`scan`] would apply.
///
/// Callers that need to know "would the agent's scan ever see this file?"
/// must ask through here rather than testing a path against `extra_ignores`
/// themselves: the ignore set is built from built-in defaults, `.gitignore`,
/// `.farhand-ignore`, and the caller's patterns, and only the matcher built
/// here is guaranteed to be the one the walk uses. This is the same reason
/// [`explain_ignore`] shares [`build_matcher`] rather than re-deriving rules.
pub fn would_ignore(root: &Path, rel_path: &str, is_dir: bool, extra_ignores: &[String]) -> bool {
    build_matcher(root, extra_ignores).should_ignore(rel_path, is_dir)
}

/// Supplies a per-file change token: a value the kernel advances on every
/// modification and that userspace cannot set back.
///
/// The digest gate compares `(size, mtime, token)`. Size and mtime alone are
/// blind to a same-size overwrite whose mtime was preserved — exactly what
/// `rsync -a`, `tar -p`, and `cp -p` produce — so a caller that has a change
/// token available should supply it. Returning 0 degrades the gate to
/// `(size, mtime)`, which is the same guarantee git makes, and is still
/// correct because a spurious *miss* only costs a re-hash.
///
/// The provider is a hook rather than an intrinsic because obtaining the token
/// needs FFI on some platforms, and this crate is `#![forbid(unsafe_code)]`.
/// The agent supplies the real implementation; see
/// `workspace::change_token`.
pub type ChangeToken<'a> = &'a dyn Fn(&Path, &std::fs::Metadata) -> i64;

/// The default provider: no change token, so the gate is `(size, mtime)`.
fn no_change_token(_path: &Path, _meta: &std::fs::Metadata) -> i64 {
    0
}

/// A file observed by the walk, before its content has been hashed.
struct Candidate {
    rel: String,
    path: PathBuf,
    size: u64,
    mode: u32,
    modified_nanos: i64,
    change_token: i64,
}

/// Counters describing how a scan spent its work, so callers can log whether
/// the stat gate actually paid off.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScanStats {
    /// Files whose digest came from the cache without reading content.
    pub reused: usize,
    /// Files whose content had to be read and hashed.
    pub hashed: usize,
}

/// A digest recorded by a previous scan, reusable only while the file's
/// `(size, mtime, ctime)` tuple is unchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedDigest {
    hash: String,
    size: u64,
    modified_nanos: i64,
    /// Defaults to 0 so an index written before this field existed still
    /// loads; those entries simply fail the `ctime` check and re-hash once.
    #[serde(default)]
    change_token: i64,
}

/// On-disk shape of the index. The stamp is deliberately *not* serialized:
/// it is always recovered from the index file's own mtime, so a caller cannot
/// construct a cache whose racy-clean guarantee does not hold.
#[derive(Debug, Default, Serialize, Deserialize)]
struct HashCacheFile {
    #[serde(default)]
    entries: HashMap<String, CachedDigest>,
}

/// Memo of digests from a previous scan, keyed by relative wire path.
///
/// Reuse is gated on `(size, mtime, ctime)`, the same shape of gate git uses,
/// with ctime closing the preserve-mtime overwrite hole that `(size, mtime)`
/// alone leaves open. One hole remains in the gate itself: a modification
/// landing in the same filesystem timestamp tick as the cached `stat` cannot
/// move the mtime, so it is invisible. [`HashCache::stamp_nanos`] closes that
/// the way git's "racily clean" check does — any entry whose mtime is not
/// *strictly older* than the moment the index was written is re-hashed, so a
/// colliding write and modification tick always resolves to a re-hash.
#[derive(Debug, Default)]
pub struct HashCache {
    stamp_nanos: i64,
    entries: HashMap<String, CachedDigest>,
}

impl HashCache {
    /// Return a cached digest for `rel`, or `None` if it must be re-hashed.
    pub fn lookup(
        &self,
        rel: &str,
        size: u64,
        modified_nanos: i64,
        change_token: i64,
    ) -> Option<&str> {
        // No stamp means the cache was never bound to a write moment, so the
        // racy-clean argument cannot be made. Fall back to re-hashing.
        if self.stamp_nanos == 0 {
            return None;
        }
        let entry = self.entries.get(rel)?;
        if entry.size != size
            || entry.modified_nanos != modified_nanos
            || entry.change_token != change_token
        {
            return None;
        }
        if modified_nanos >= self.stamp_nanos {
            return None;
        }
        Some(&entry.hash)
    }

    /// Number of digests currently held.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache holds no digests.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Name of the digest index, written into a workspace root.
pub const HASH_INDEX_FILENAME: &str = ".farhand-hashindex.json";

/// Load a digest index from `path` and bind it to that file's own mtime.
///
/// A missing, unreadable, or malformed index yields an empty cache, which
/// simply re-hashes everything — a corrupt index costs time, never
/// correctness.
pub fn load_hash_cache(path: &Path) -> HashCache {
    let stamp_nanos = match std::fs::metadata(path)
        .and_then(|m| m.modified())
        .and_then(|t| t.duration_since(UNIX_EPOCH).map_err(std::io::Error::other))
    {
        Ok(d) => d.as_nanos() as i64,
        Err(_) => 0,
    };

    let entries = std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<HashCacheFile>(&raw).ok())
        .map(|f| f.entries)
        .unwrap_or_default();

    HashCache {
        stamp_nanos,
        entries,
    }
}

/// Write a digest index to `path` atomically, so a crash mid-write cannot
/// leave a truncated index that later parses as a short entry list.
pub fn save_hash_cache(path: &Path, cache: &HashCache) -> std::io::Result<()> {
    let body = serde_json::to_vec(&HashCacheFile {
        entries: cache.entries.clone(),
    })
    .map_err(std::io::Error::other)?;

    let nanos = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = path.with_file_name(format!(
        "{}.tmp.{}.{}",
        HASH_INDEX_FILENAME,
        std::process::id(),
        nanos
    ));

    {
        let mut f = File::create(&tmp)?;
        f.write_all(&body)?;
        f.sync_all()?;
    }

    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Walk `root`, stat every surviving file, then hash in parallel.
///
/// Splitting the walk from the hashing is what makes the stat gate possible:
/// by the time a digest is needed, the file's `(size, mtime)` is already known,
/// so an unchanged file never has its content read.
fn scan_candidates(
    root: &Path,
    extra_ignores: &[String],
    change_token: ChangeToken<'_>,
) -> Result<Vec<Candidate>, FilesetError> {
    let matcher = build_matcher(root, extra_ignores);

    let mut candidates = Vec::new();
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
            let modified_nanos = metadata
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as i64)
                .unwrap_or(0);

            candidates.push(Candidate {
                rel: rel_wire_path,
                path: entry_path.to_path_buf(),
                size: metadata.len(),
                mode: get_file_mode(&metadata),
                modified_nanos,
                change_token: change_token(entry_path, &metadata),
            });
        }
    }

    Ok(candidates)
}

/// Scan a directory, reusing digests from `cache` where the file is unchanged.
///
/// On return `cache` holds exactly one entry per file the scan observed, so
/// files that disappeared since the last scan are pruned automatically.
pub fn scan_cached(
    root: &Path,
    extra_ignores: &[String],
    cache: &mut HashCache,
) -> Result<(HashMap<String, FileMeta>, ScanStats), FilesetError> {
    scan_cached_with(root, extra_ignores, cache, &no_change_token)
}

/// [`scan_cached`], with a caller-supplied change token.
pub fn scan_cached_with(
    root: &Path,
    extra_ignores: &[String],
    cache: &mut HashCache,
    change_token: ChangeToken<'_>,
) -> Result<(HashMap<String, FileMeta>, ScanStats), FilesetError> {
    let candidates = scan_candidates(root, extra_ignores, change_token)?;

    // Resolve every digest before rebuilding the cache, so the borrow of
    // `cache` taken by the lookups ends before it is overwritten below.
    let mut resolved: Vec<Option<String>> = Vec::with_capacity(candidates.len());
    let mut reused = 0usize;
    let mut hashed = 0usize;

    for c in &candidates {
        match cache.lookup(&c.rel, c.size, c.modified_nanos, c.change_token) {
            Some(h) => {
                reused += 1;
                resolved.push(Some(h.to_string()));
            }
            None => {
                hashed += 1;
                resolved.push(None);
            }
        }
    }

    // Hash only the misses, across all cores. `collect` on an indexed rayon
    // iterator preserves order, so the miss stream lines up with the misses in
    // `resolved`. A read failure aborts the whole scan, matching the
    // pre-existing behaviour of the inline loop.
    let mut computed = candidates
        .par_iter()
        .zip(resolved.par_iter())
        .map(|(c, known)| match known {
            Some(_) => None,
            None => Some(hash_file(&c.path)),
        })
        .collect::<Vec<_>>()
        .into_iter()
        .flatten();

    let mut result = HashMap::with_capacity(candidates.len());
    let mut next_entries = HashMap::with_capacity(candidates.len());

    for (c, known) in candidates.into_iter().zip(resolved) {
        let hash = match known {
            Some(h) => h,
            None => match computed.next() {
                Some(Ok(h)) => h,
                Some(Err(e)) => return Err(FilesetError::Io(e)),
                // Unreachable: one digest is produced per miss above. Handled
                // as an error rather than a panic so a future edit to the
                // filter cannot abort the daemon.
                None => {
                    return Err(FilesetError::Io(std::io::Error::other(
                        "digest stream shorter than the miss count",
                    )))
                }
            },
        };

        next_entries.insert(
            c.rel.clone(),
            CachedDigest {
                hash: hash.clone(),
                size: c.size,
                modified_nanos: c.modified_nanos,
                change_token: c.change_token,
            },
        );
        result.insert(
            c.rel.clone(),
            FileMeta {
                path: c.rel,
                hash,
                size: c.size,
                mode: c.mode,
                modified_nanos: c.modified_nanos,
            },
        );
    }

    // Re-stamp to the instant this scan finished. That is the in-memory
    // equivalent of the index file's own mtime: the moment we recorded our
    // view of the tree. Any file whose mtime is not strictly older than this
    // could have been modified in the same tick we hashed it, so the next
    // lookup re-hashes it.
    let stamp_nanos = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);

    *cache = HashCache {
        stamp_nanos,
        entries: next_entries,
    };

    Ok((result, ScanStats { reused, hashed }))
}

pub fn scan(
    root: &Path,
    extra_ignores: &[String],
) -> Result<HashMap<String, FileMeta>, FilesetError> {
    let (files, _) = scan_cached(root, extra_ignores, &mut HashCache::default())?;
    Ok(files)
}

/// [`scan`], reusing digests this process already computed for the same root.
pub fn scan_shared(
    root: &Path,
    extra_ignores: &[String],
) -> Result<HashMap<String, FileMeta>, FilesetError> {
    use std::collections::HashMap as Map;
    static CACHES: std::sync::OnceLock<std::sync::Mutex<Map<PathBuf, HashCache>>> =
        std::sync::OnceLock::new();
    let caches = CACHES.get_or_init(|| std::sync::Mutex::new(Map::new()));

    let mut guard = caches.lock().unwrap_or_else(|e| e.into_inner());
    let cache = guard.entry(root.to_path_buf()).or_default();
    let (files, _) = scan_cached(root, extra_ignores, cache)?;
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    /// The exclusion `workspace::diff_manifests` always applies: the index
    /// lives inside the scanned root, so it must not scan as a project file.
    fn index_excluded() -> Vec<String> {
        vec![HASH_INDEX_FILENAME.to_string()]
    }
    fn write(root: &Path, name: &str, body: &str) {
        let p = root.join(name);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, body).unwrap();
    }

    /// Overwrite with different content but *identical byte length*, then put
    /// the original mtime back. This is the case a `(size, mtime)` gate alone
    /// cannot see; the ctime in the gate is what catches it.
    fn overwrite_preserving_mtime(path: &Path, body: &str, mtime: std::time::SystemTime) {
        fs::write(path, body).unwrap();
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
    }

    #[test]
    fn second_scan_reuses_every_digest_when_nothing_changed() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        for i in 0..5 {
            write(root, &format!("f{i}.txt"), &"body".repeat(i + 1));
        }

        let mut cache = HashCache::default();
        let (first, s1) = scan_cached(root, &[], &mut cache).unwrap();
        assert_eq!(
            s1,
            ScanStats {
                reused: 0,
                hashed: 5
            }
        );

        let (second, s2) = scan_cached(root, &[], &mut cache).unwrap();
        assert_eq!(
            s2,
            ScanStats {
                reused: 5,
                hashed: 0
            }
        );

        // Reuse must be invisible in the result: the digests are identical to
        // a cache-free scan.
        assert_eq!(first, second);
        assert_eq!(second, scan(root, &[]).unwrap());
    }

    #[test]
    fn edited_file_is_rehashed_and_cache_reports_the_miss() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        write(root, "a.txt", "original");
        write(root, "b.txt", "untouched");

        let mut cache = HashCache::default();
        let (before, _) = scan_cached(root, &[], &mut cache).unwrap();
        let original = before.get("a.txt").unwrap().hash.clone();

        write(root, "a.txt", "edited!!");

        let (after, stats) = scan_cached(root, &[], &mut cache).unwrap();
        assert_eq!(
            stats,
            ScanStats {
                reused: 1,
                hashed: 1
            }
        );
        assert_ne!(after.get("a.txt").unwrap().hash, original);
        assert_eq!(
            after.get("a.txt").unwrap().hash,
            hash_file(&root.join("a.txt")).unwrap()
        );
    }

    #[test]
    fn deleted_file_is_pruned_from_the_cache() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        write(root, "a.txt", "a");
        write(root, "b.txt", "b");

        let mut cache = HashCache::default();
        scan_cached(root, &[], &mut cache).unwrap();
        assert_eq!(cache.len(), 2);

        fs::remove_file(root.join("b.txt")).unwrap();
        let (files, _) = scan_cached(root, &[], &mut cache).unwrap();

        assert_eq!(files.len(), 1);
        assert_eq!(cache.len(), 1, "stale entry was not pruned");
    }

    /// The provider hook is what lets the agent supply a real change token. This
    /// pins the mechanism the gate depends on: a token that moves forces a
    /// re-hash even when the size *and* the mtime are both identical — the
    /// exact shape of the `rsync -a` / `tar -p` blind spot.
    #[test]
    fn a_moved_change_token_forces_a_rehash_though_size_and_mtime_match() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        write(root, "a.txt", "aaaa");
        let path = root.join("a.txt");

        let before_mtime = fs::metadata(&path).unwrap().modified().unwrap();

        // A provider standing in for the platform token, held in a cell so a
        // test can move it between scans.
        let token = std::cell::Cell::new(1i64);
        let provider = |_: &Path, _: &std::fs::Metadata| token.get();

        let mut cache = HashCache::default();
        let (first, s1) = scan_cached_with(root, &[], &mut cache, &provider).unwrap();
        assert_eq!(
            s1,
            ScanStats {
                reused: 0,
                hashed: 1
            }
        );

        // Same size, and the mtime rolled back to the identical value.
        overwrite_preserving_mtime(&path, "bbbb", before_mtime);
        token.set(2);

        let (after, s2) = scan_cached_with(root, &[], &mut cache, &provider).unwrap();
        assert_eq!(
            s2,
            ScanStats {
                reused: 0,
                hashed: 1
            },
            "a moved change token was ignored"
        );
        assert_ne!(
            after.get("a.txt").unwrap().hash,
            first.get("a.txt").unwrap().hash
        );
        assert_eq!(after.get("a.txt").unwrap().hash, hash_file(&path).unwrap());
    }

    /// The complementary half: a token that stays put must not cost a re-hash,
    /// or the hook would be a pessimisation.
    #[test]
    fn a_stable_change_token_still_reuses() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        write(root, "a.txt", "content");

        let provider = |_: &Path, meta: &std::fs::Metadata| meta.len() as i64;

        let mut cache = HashCache::default();
        let (_, first) = scan_cached_with(root, &[], &mut cache, &provider).unwrap();
        assert_eq!(
            first,
            ScanStats {
                reused: 0,
                hashed: 1
            }
        );

        let (files, second) = scan_cached_with(root, &[], &mut cache, &provider).unwrap();
        assert_eq!(
            second,
            ScanStats {
                reused: 1,
                hashed: 0
            }
        );
        assert_eq!(
            files.get("a.txt").unwrap().hash,
            hash_file(&root.join("a.txt")).unwrap()
        );
    }

    /// With no provider the gate is `(size, mtime)`, so a same-size rewrite
    /// that preserves the mtime is invisible — git has the same limit. This
    /// test exists so that degradation is a decision on record rather than an
    /// accident.
    #[test]
    fn without_a_provider_the_gate_degrades_to_size_and_mtime() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        write(root, "a.txt", "aaaa");
        let path = root.join("a.txt");
        let before_mtime = fs::metadata(&path).unwrap().modified().unwrap();

        let mut cache = HashCache::default();
        let (before, _) = scan_cached(root, &[], &mut cache).unwrap();
        let original = before.get("a.txt").unwrap().hash.clone();

        overwrite_preserving_mtime(&path, "bbbb", before_mtime);

        let (after, stats) = scan_cached(root, &[], &mut cache).unwrap();
        assert_eq!(
            stats,
            ScanStats {
                reused: 1,
                hashed: 0
            }
        );
        assert_eq!(
            after.get("a.txt").unwrap().hash,
            original,
            "(size, mtime) alone cannot see this — hence the provider"
        );
    }

    #[test]
    fn index_round_trips_across_processes() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        write(root, "a.txt", "content");
        let index = root.join(HASH_INDEX_FILENAME);

        let mut first = HashCache::default();
        let (files, _) = scan_cached(root, &index_excluded(), &mut first).unwrap();
        save_hash_cache(&index, &first).unwrap();

        // A fresh process would load the index from disk rather than inherit
        // the in-memory map.
        let mut second = load_hash_cache(&index);
        let (reloaded, stats) = scan_cached(root, &index_excluded(), &mut second).unwrap();

        assert_eq!(
            stats,
            ScanStats {
                reused: 1,
                hashed: 0
            }
        );
        assert_eq!(files, reloaded);
    }

    #[test]
    fn corrupt_index_degrades_to_a_full_rehash() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        write(root, "a.txt", "content");
        let index = root.join(HASH_INDEX_FILENAME);
        fs::write(&index, b"{ this is not json").unwrap();

        let mut cache = load_hash_cache(&index);
        let (files, stats) = scan_cached(root, &index_excluded(), &mut cache).unwrap();

        assert_eq!(
            stats,
            ScanStats {
                reused: 0,
                hashed: 1
            }
        );
        assert_eq!(
            files.get("a.txt").unwrap().hash,
            hash_file(&root.join("a.txt")).unwrap()
        );
    }

    #[test]
    fn the_index_never_appears_in_its_own_scan_results() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        write(root, "a.txt", "content");
        let index = root.join(HASH_INDEX_FILENAME);

        let mut cache = HashCache::default();
        scan_cached(root, &index_excluded(), &mut cache).unwrap();
        save_hash_cache(&index, &cache).unwrap();

        let mut reloaded = load_hash_cache(&index);
        let (files, stats) = scan_cached(root, &index_excluded(), &mut reloaded).unwrap();

        assert_eq!(
            stats,
            ScanStats {
                reused: 1,
                hashed: 0
            }
        );
        assert!(!files.contains_key(HASH_INDEX_FILENAME));
    }

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

    #[test]
    fn explain_ignore_names_the_rule_that_matched() {
        let root = tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::create_dir_all(root.path().join("target/debug")).unwrap();
        std::fs::write(root.path().join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(root.path().join("debug.log"), "noise").unwrap();
        std::fs::write(root.path().join("target/debug/bin"), "x").unwrap();
        std::fs::write(root.path().join(".gitignore"), "*.log\n").unwrap();

        // A .gitignore pattern is reported by name.
        assert_eq!(
            explain_ignore(root.path(), "debug.log", &[]).as_deref(),
            Some("ignore rule: *.log")
        );

        // The built-in list is reported as such, with the component that hit.
        assert_eq!(
            explain_ignore(root.path(), "target/debug/bin", &[]).as_deref(),
            Some("built-in ignore: target/")
        );

        // A plain source file is not ignored — that is not this function's
        // answer to give, and returning None says so.
        assert_eq!(explain_ignore(root.path(), "src/main.rs", &[]), None);

        // Extra patterns are honored exactly as the scan applies them.
        assert_eq!(
            explain_ignore(root.path(), "src/main.rs", &["*.rs".to_string()]).as_deref(),
            Some("ignore rule: *.rs")
        );
    }
}
