use crate::messages::MsgType;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Maximum allowed payload size per frame (256 MB)
pub const MAX_PAYLOAD_SIZE: usize = 256 * 1024 * 1024;

#[derive(Error, Debug)]
pub enum FrameError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Payload length {0} exceeds maximum allowed size of {1} bytes")]
    PayloadTooLarge(usize, usize),

    #[error("Invalid or unrecognized message type: 0x{0:02x}")]
    InvalidMsgType(u8),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Unexpected end-of-file while reading frame")]
    UnexpectedEof,
}

/// Write a binary frame to an async writer.
///
/// Layout:
/// - 1 byte MsgType
/// - 4 bytes uint32 Big-Endian PayloadLength
/// - Payload bytes
pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    msg_type: MsgType,
    payload: &[u8],
) -> Result<(), FrameError> {
    if payload.len() > MAX_PAYLOAD_SIZE {
        return Err(FrameError::PayloadTooLarge(payload.len(), MAX_PAYLOAD_SIZE));
    }

    let mut header = [0u8; 5];
    header[0] = msg_type.to_u8();
    let length = payload.len() as u32;
    header[1..5].copy_from_slice(&length.to_be_bytes());

    writer.write_all(&header).await?;
    if !payload.is_empty() {
        writer.write_all(payload).await?;
    }
    writer.flush().await?;
    Ok(())
}

/// Read a binary frame from an async reader.
pub async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<(MsgType, Vec<u8>), FrameError> {
    let mut header = [0u8; 5];
    match reader.read_exact(&mut header).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Err(FrameError::UnexpectedEof);
        }
        Err(e) => return Err(FrameError::Io(e)),
    }

    let msg_type = MsgType::from_u8(header[0]).ok_or(FrameError::InvalidMsgType(header[0]))?;

    let length = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
    if length > MAX_PAYLOAD_SIZE {
        return Err(FrameError::PayloadTooLarge(length, MAX_PAYLOAD_SIZE));
    }

    let mut payload = vec![0u8; length];
    if length > 0 {
        match reader.read_exact(&mut payload).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Err(FrameError::UnexpectedEof);
            }
            Err(e) => return Err(FrameError::Io(e)),
        }
    }

    Ok((msg_type, payload))
}

/// Serialize a Rust data structure to JSON and write it as a frame.
pub async fn write_json_frame<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    msg_type: MsgType,
    value: &T,
) -> Result<(), FrameError> {
    let bytes = serde_json::to_vec(value)?;
    write_frame(writer, msg_type, &bytes).await
}

/// Deserialize a JSON payload from a frame.
pub fn decode_json<T: for<'de> Deserialize<'de>>(payload: &[u8]) -> Result<T, FrameError> {
    Ok(serde_json::from_slice(payload)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[tokio::test]
    async fn test_frame_roundtrip_bytes() {
        let mut buffer = Vec::new();
        let payload = b"hello from farhand wire protocol";
        write_frame(&mut buffer, MsgType::Hello, payload)
            .await
            .unwrap();

        let mut cursor = Cursor::new(buffer);
        let (msg_type, read_payload) = read_frame(&mut cursor).await.unwrap();

        assert_eq!(msg_type, MsgType::Hello);
        assert_eq!(read_payload, payload);
    }

    #[tokio::test]
    async fn test_empty_payload() {
        let mut buffer = Vec::new();
        write_frame(&mut buffer, MsgType::Files, &[]).await.unwrap();

        assert_eq!(buffer.len(), 5);
        assert_eq!(buffer[0], MsgType::Files.to_u8());
        assert_eq!(&buffer[1..5], &[0, 0, 0, 0]);

        let mut cursor = Cursor::new(buffer);
        let (msg_type, read_payload) = read_frame(&mut cursor).await.unwrap();
        assert_eq!(msg_type, MsgType::Files);
        assert!(read_payload.is_empty());
    }

    #[tokio::test]
    async fn test_json_frame_roundtrip() {
        use crate::messages::HelloPayload;

        let payload = HelloPayload {
            token: "secret-token".to_string(),
            project: "my-crate".to_string(),
            protocol_version: 1,
            compressions: None,
        };

        let mut buffer = Vec::new();
        write_json_frame(&mut buffer, MsgType::Hello, &payload)
            .await
            .unwrap();

        let mut cursor = Cursor::new(buffer);
        let (msg_type, raw_bytes) = read_frame(&mut cursor).await.unwrap();
        assert_eq!(msg_type, MsgType::Hello);

        let decoded: HelloPayload = decode_json(&raw_bytes).unwrap();
        assert_eq!(decoded, payload);
    }

    #[tokio::test]
    async fn test_invalid_msg_type() {
        let bad_header = [0xFF, 0x00, 0x00, 0x00, 0x00];
        let mut cursor = Cursor::new(bad_header);
        let err = read_frame(&mut cursor).await.unwrap_err();
        match err {
            FrameError::InvalidMsgType(val) => assert_eq!(val, 0xFF),
            _ => panic!("expected InvalidMsgType, got {:?}", err),
        }
    }

    #[tokio::test]
    async fn test_partial_header_unexpected_eof() {
        let partial_header = [0x01, 0x00]; // Only 2 bytes instead of 5
        let mut cursor = Cursor::new(partial_header);
        let err = read_frame(&mut cursor).await.unwrap_err();
        match err {
            FrameError::UnexpectedEof => {}
            _ => panic!("expected UnexpectedEof, got {:?}", err),
        }
    }

    #[tokio::test]
    async fn test_partial_payload_unexpected_eof() {
        let mut buffer = vec![0x01, 0x00, 0x00, 0x00, 0x0A]; // declares 10 bytes payload
        buffer.extend_from_slice(b"123"); // only provides 3 bytes
        let mut cursor = Cursor::new(buffer);
        let err = read_frame(&mut cursor).await.unwrap_err();
        match err {
            FrameError::UnexpectedEof => {}
            _ => panic!("expected UnexpectedEof, got {:?}", err),
        }
    }

    #[tokio::test]
    async fn test_payload_exceeds_max() {
        let oversized = MAX_PAYLOAD_SIZE + 1;

        // write_frame check
        // simulate oversized length without allocating 256MB in memory:
        let mut header = [0u8; 5];
        header[0] = MsgType::Files.to_u8();
        header[1..5].copy_from_slice(&(oversized as u32).to_be_bytes());

        let mut cursor = Cursor::new(header);
        let err = read_frame(&mut cursor).await.unwrap_err();
        match err {
            FrameError::PayloadTooLarge(len, max) => {
                assert_eq!(len, oversized);
                assert_eq!(max, MAX_PAYLOAD_SIZE);
            }
            _ => panic!("expected PayloadTooLarge, got {:?}", err),
        }
    }
}
