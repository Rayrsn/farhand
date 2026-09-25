// Filesystem scanning and archive handling stay pure safe Rust — in
// particular, the tar unpacker's zip-slip defenses are auditable without
// reasoning about `unsafe` invariants.
#![forbid(unsafe_code)]

pub mod ignore;
pub mod scan;
pub mod tar;

pub use ignore::{is_default_ignored, IgnoreMatcher, IgnoreRule, DEFAULT_IGNORES};
pub use scan::{get_file_mode, hash_file, scan, FileMeta, FilesetError};
pub use tar::{pack_tar, pack_tar_with_algo, unpack_tar, CompressionAlgo};
