use protocol::{
    decode_json, read_frame, write_json_frame, CleanRequestPayload, CleanResponsePayload,
    HelloAckPayload, MsgType,
};
use tokio::net::TcpStream;

/// Sends a CLEAN request to the remote agent daemon.
pub async fn clean_workspace(
    host: &str,
    token: &str,
    project: &str,
    all_branches: bool,
    caches_only: bool,
) -> Result<CleanResponsePayload, Box<dyn std::error::Error>> {
    let mut stream = TcpStream::connect(host).await?;
    let req = CleanRequestPayload {
        token: token.to_string(),
        project: project.to_string(),
        all_branches,
        caches_only,
    };
    write_json_frame(&mut stream, MsgType::Clean, &req).await?;

    let (msg_type, payload) = read_frame(&mut stream).await?;
    if msg_type == MsgType::HelloAck {
        let ack: HelloAckPayload = decode_json(&payload)?;
        return Err(ack
            .error
            .unwrap_or_else(|| "Unauthorized clean request".into())
            .into());
    }
    if msg_type != MsgType::CleanResp {
        return Err(format!("Expected CLEAN_RESP frame, got {:?}", msg_type).into());
    }

    let resp: CleanResponsePayload = decode_json(&payload)?;
    Ok(resp)
}
