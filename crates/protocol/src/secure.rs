//! Authentication primitives shared by client and daemon.
//!
//! Token comparison is performed in constant time over fixed-size SHA-256
//! digests: hashing normalizes the inputs to a fixed 32-byte length (so
//! relative lengths and common prefixes do not leak), and the digest
//! comparison folds XOR differences into a single accumulator bit so it
//! cannot short-circuit.

use sha2::{Digest, Sha256};

/// Constant-time equality of two byte strings, compared via their SHA-256
/// digests so that no information about the inputs (length, matching prefix)
/// is observable through timing.
pub fn ct_eq_bytes(a: &[u8], b: &[u8]) -> bool {
    let da: [u8; 32] = Sha256::digest(a).into();
    let db: [u8; 32] = Sha256::digest(b).into();

    let mut acc: u8 = 0;
    for (x, y) in da.iter().zip(db.iter()) {
        acc |= x ^ y;
    }
    acc == 0
}

/// Constant-time check of a provided token against the expected token.
///
/// - `(None, None)` → authorized (unauthenticated mode is explicitly configured
///   on the daemon).
/// - `Some(expected)` requires `Some(provided)` to match in constant time.
pub fn ct_eq_tokens(provided: Option<&str>, expected: Option<&str>) -> bool {
    match (provided, expected) {
        (Some(p), Some(e)) => ct_eq_bytes(p.as_bytes(), e.as_bytes()),
        (None, None) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ct_eq_bytes_matches_equal_inputs() {
        assert!(ct_eq_bytes(b"secret-token", b"secret-token"));
        assert!(ct_eq_bytes(b"", b""));
    }

    #[test]
    fn ct_eq_bytes_rejects_unequal_inputs() {
        // Every single-byte difference must be caught.
        let token = b"correct-horse-battery-staple";
        for i in 0..token.len() {
            let mut tampered = *token;
            tampered[i] ^= 0x20;
            assert!(!ct_eq_bytes(token, &tampered), "byte {i} mismatch leaked");
        }
        assert!(!ct_eq_bytes(b"secret", b"different"));
    }

    #[test]
    fn ct_eq_tokens_handles_optional_inputs() {
        assert!(ct_eq_tokens(Some("tok"), Some("tok")));
        assert!(!ct_eq_tokens(Some("tok"), Some("other")));
        assert!(!ct_eq_tokens(Some("tok"), None));
        assert!(!ct_eq_tokens(None, Some("tok")));
        assert!(ct_eq_tokens(None, None));
    }

    #[test]
    fn ct_eq_bytes_is_length_normalized() {
        // Same digest prefix: differing lengths must not compare equal.
        assert!(!ct_eq_bytes(b"short", b"short-short-short"));
        assert!(!ct_eq_bytes(b"", b"x"));
    }
}
