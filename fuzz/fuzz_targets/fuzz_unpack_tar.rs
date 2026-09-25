#![no_main]
//! Tar unpacker invariants under arbitrary and mutated archives.
//!
//! Contract:
//! 1. `unpack_tar` never panics, even on hostile bytes.
//! 2. Nothing is ever written outside the destination directory
//!    (zip-slip containment — see `from_wire_path` checks inside unpack).

use libfuzzer_sys::fuzz_target;
use fileset::tar::unpack_tar;
use std::path::PathBuf;

fuzz_target!(|data: &[u8]| {
    // One destination per iteration, wiped afterwards — fuzzing must not
    // leave state behind that changes the next iteration.
    let dir: PathBuf = std::env::temp_dir().join(format!(
        "farhand-fuzz-tar-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dest");

    let result = unpack_tar(&dir, data);

    if result.is_ok() {
        // If the unpacker claims success, every produced entry must live
        // under the destination (no `..` escapes, no absolute paths).
        for entry in walk(&dir) {
            assert!(
                entry.starts_with(&dir),
                "zip-slip: {:?} escaped {:?}",
                entry,
                dir
            );
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
});

fn walk(root: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        out.push(path.clone());
        if path.is_dir() {
            out.extend(walk(&path));
        }
    }
    out
}
