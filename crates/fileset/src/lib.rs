// Filesystem scanning and archive handling stay pure safe Rust — in
// particular, the tar unpacker's zip-slip defenses are auditable without
// reasoning about `unsafe` invariants.
#![forbid(unsafe_code)]

pub mod ignore;
pub mod scan;
pub mod tar;

pub use ignore::{is_default_ignored, IgnoreMatcher, IgnoreRule, DEFAULT_IGNORES};
pub use scan::would_ignore;
pub use scan::{
    explain_ignore, get_file_mode, hash_file, load_hash_cache, save_hash_cache, scan, scan_cached,
    scan_cached_with, scan_shared, ChangeToken, FileMeta, FilesetError, HashCache, ScanStats,
    HASH_INDEX_FILENAME,
};
pub use tar::{
    pack_tar, pack_tar_with_algo, pack_tar_with_algo_progress, unpack_tar, unpack_tar_limited,
    CompressionAlgo, DEFAULT_MAX_UNPACKED_BYTES,
};
