// The wire layer is pure safe Rust: parsing, framing, and path normalization
// must never contain unsafe code, so any attempt to add some fails to compile.
#![forbid(unsafe_code)]

pub mod frame;
pub mod messages;
pub mod path;
pub mod secure;
pub mod tls;

pub use frame::{
    decode_json, read_frame, read_frame_limited, write_frame, write_json_frame, FrameError,
    MAX_PAYLOAD_SIZE, MAX_PRE_AUTH_PAYLOAD,
};
pub use messages::*;
pub use path::{from_wire_path, to_wire_path};
pub use secure::{ct_eq_bytes, ct_eq_tokens};
pub use tls::*;
