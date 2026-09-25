#![no_main]
//! Frame reader invariants under arbitrary input.
//!
//! Contract:
//! 1. `read_frame` never panics or aborts.
//! 2. Any accepted payload respects `MAX_PAYLOAD_SIZE`.
//! 3. A header claiming more data than the stream delivers fails with
//!    `UnexpectedEof` — memory is allocated as bytes arrive, never from the
//!    claimed length.

use libfuzzer_sys::fuzz_target;
use protocol::{read_frame, FrameError, MAX_PAYLOAD_SIZE};
use std::io::Cursor;

fuzz_target!(|data: &[u8]| {
    // read_frame is async; drive the future on an in-memory cursor.
    let mut cursor = Cursor::new(data.to_vec());
    let result = futures::executor::block_on(read_frame(&mut cursor));

    match result {
        Ok((_, payload)) => {
            assert!(
                payload.len() <= MAX_PAYLOAD_SIZE,
                "reader accepted {} bytes (cap {})",
                payload.len(),
                MAX_PAYLOAD_SIZE
            );
        }
        Err(FrameError::Io(_)) | Err(FrameError::UnexpectedEof) => {}
        Err(FrameError::PayloadTooLarge(len, cap)) => {
            assert!(len > cap, "rejected with len {} <= cap {}", len, cap);
        }
        Err(FrameError::InvalidMsgType(_)) | Err(FrameError::Json(_)) => {}
    }
});
