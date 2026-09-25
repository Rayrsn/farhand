#![no_main]
//! Wire-path parser invariants under hostile strings.
//!
//! Contract: anything `from_wire_path` accepts must be a *relative* path
//! with no `..` component and no backslash — the protocol-level zip-slip
//! gate (AGENTS.md §3.1/§3.2).

use libfuzzer_sys::fuzz_target;
use protocol::from_wire_path;

fuzz_target!(|data: &[u8]| {
    // Only valid UTF-8 reaches the parser in production (the wire format is
    // JSON strings), so decode lossily and fuzz the string space.
    let candidate = String::from_utf8_lossy(data);

    if let Ok(path) = from_wire_path(&candidate) {
        assert!(!path.is_absolute(), "accepted absolute path: {:?}", candidate);
        let normalized = path.to_string_lossy().replace('\\', "/");
        for component in normalized.split('/') {
            assert_ne!(component, "..", "accepted traversal: {:?}", candidate);
        }
        assert!(
            !normalized.contains('\\'),
            "accepted backslash path: {:?}",
            candidate
        );
    }
});
