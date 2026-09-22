use config::TlsConfig;
use protocol::{
    decode_json, read_frame, write_frame, write_json_frame, HelloAckPayload, HelloPayload,
    LogPayload, ManifestPayload, MsgType, NeedPayload, ResultPayload, RunPayload,
    CURRENT_PROTOCOL_VERSION,
};
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;

/// Converts a local filesystem path to a standard file:// URI.
pub fn path_to_file_uri(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let trimmed = s.trim_start_matches('/');
    format!("file:///{}", trimmed)
}

/// Translates occurrences of `from_prefix` to `to_prefix` inside an LSP JSON string.
pub fn translate_uri_string(json_text: &str, from_prefix: &str, to_prefix: &str) -> String {
    json_text.replace(from_prefix, to_prefix)
}

/// Encodes an LSP JSON-RPC message body with standard Content-Length headers.
pub fn encode_lsp_message(body: &str) -> Vec<u8> {
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    let mut out = Vec::with_capacity(header.len() + body.len());
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(body.as_bytes());
    out
}

/// A streaming buffer that accumulates bytes and extracts complete LSP JSON-RPC messages.
#[derive(Default)]
pub struct LspStreamParser {
    buffer: Vec<u8>,
}

impl LspStreamParser {
    pub fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    pub fn feed(&mut self, chunk: &[u8]) {
        self.buffer.extend_from_slice(chunk);
    }

    /// Attempts to extract the next complete JSON-RPC message from the buffer.
    pub fn next_message(&mut self) -> Option<String> {
        let needle = b"\r\n\r\n";
        let alt_needle = b"\n\n";

        let (header_end, delimiter_len) =
            if let Some(pos) = self.buffer.windows(needle.len()).position(|w| w == needle) {
                (pos, 4)
            } else {
                let pos = self
                    .buffer
                    .windows(alt_needle.len())
                    .position(|w| w == alt_needle)?;
                (pos, 2)
            };

        let header_str = String::from_utf8_lossy(&self.buffer[..header_end]);
        let mut content_length = None;
        for line in header_str.lines() {
            if let Some(rest) = line.strip_prefix("Content-Length:") {
                if let Ok(len) = rest.trim().parse::<usize>() {
                    content_length = Some(len);
                    break;
                }
            } else if let Some(rest) = line.strip_prefix("content-length:") {
                if let Ok(len) = rest.trim().parse::<usize>() {
                    content_length = Some(len);
                    break;
                }
            }
        }

        let len = content_length?;
        let body_start = header_end + delimiter_len;
        if self.buffer.len() < body_start + len {
            return None; // Need more data
        }

        let body_bytes = self.buffer[body_start..body_start + len].to_vec();
        self.buffer.drain(..body_start + len);
        String::from_utf8(body_bytes).ok()
    }
}

/// Synchronizes local project files with the remote agent workspace.
async fn sync_project<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    stream: &mut S,
    local_dir: &Path,
    compression: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let scanned_files = fileset::scan(local_dir, &[])?;
    let manifest_files: Vec<protocol::FileEntry> = scanned_files
        .values()
        .map(|meta| protocol::FileEntry {
            path: meta.path.clone(),
            hash: meta.hash.clone(),
            size: meta.size,
            mode: meta.mode,
        })
        .collect();
    let manifest = ManifestPayload {
        files: manifest_files,
    };
    write_json_frame(stream, MsgType::Manifest, &manifest).await?;

    let (msg_type, payload) = read_frame(stream).await?;
    if msg_type != MsgType::Need {
        return Err(format!("Expected NEED frame during sync, got {:?}", msg_type).into());
    }
    let need: NeedPayload = decode_json(&payload)?;

    let algo = fileset::CompressionAlgo::from_str_opt(Some(compression));
    let tar_bytes = if need.want.is_empty() {
        Vec::new()
    } else {
        fileset::pack_tar_with_algo(local_dir, &need.want, algo)?
    };

    write_frame(stream, MsgType::Files, &tar_bytes).await?;
    Ok(())
}

/// Synchronizes a single saved file to the agent workspace.
pub async fn sync_single_file(
    host: &str,
    token: &str,
    project: &str,
    local_dir: &Path,
    rel_path: &str,
    tls_config: Option<&TlsConfig>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let local_file = local_dir.join(rel_path);
    if !local_file.is_file() {
        return Ok(());
    }

    let mut stream = crate::connect_to_agent(host, tls_config).await?;
    let hello = HelloPayload {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        token: token.to_string(),
        project: project.to_string(),
        compressions: Some(vec!["zstd".into(), "gzip".into()]),
    };
    write_json_frame(&mut stream, MsgType::Hello, &hello).await?;

    let (msg_type, payload) = read_frame(&mut stream).await?;
    if msg_type != MsgType::HelloAck {
        return Err("Handshake failed".into());
    }
    let ack: HelloAckPayload = decode_json(&payload)?;
    if !ack.ok {
        return Err("Agent rejected handshake".into());
    }

    let comp = ack.compression.unwrap_or_else(|| "gzip".into());
    let algo = fileset::CompressionAlgo::from_str_opt(Some(&comp));
    let wire_path = protocol::to_wire_path(Path::new(rel_path));
    let tar_bytes = fileset::pack_tar_with_algo(local_dir, &[wire_path], algo)?;

    let hash = fileset::hash_file(&local_file).unwrap_or_default();
    let size = std::fs::metadata(&local_file).map(|m| m.len()).unwrap_or(0);
    let manifest = ManifestPayload {
        files: vec![protocol::FileEntry {
            path: rel_path.to_string(),
            hash,
            size,
            mode: 0o644,
        }],
    };
    write_json_frame(&mut stream, MsgType::Manifest, &manifest).await?;

    let (msg_type, _payload) = read_frame(&mut stream).await?;
    if msg_type != MsgType::Need {
        return Err("Expected NEED frame".into());
    }

    write_frame(&mut stream, MsgType::Files, &tar_bytes).await?;
    Ok(())
}

/// Runs the remote LSP bridge, forwarding raw JSON-RPC stdio with path translation.
#[allow(clippy::too_many_arguments)]
pub async fn run_lsp(
    host: &str,
    token: &str,
    project: &str,
    local_dir: &Path,
    lsp_command: &[String],
    no_sync: bool,
    no_save_sync: bool,
    tls_config: Option<&TlsConfig>,
) -> Result<i32, Box<dyn std::error::Error + Send + Sync>> {
    let mut stream = crate::connect_to_agent(host, tls_config).await?;

    let hello = HelloPayload {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        token: token.to_string(),
        project: project.to_string(),
        compressions: Some(vec!["zstd".into(), "gzip".into(), "none".into()]),
    };
    write_json_frame(&mut stream, MsgType::Hello, &hello).await?;

    let (msg_type, payload) = read_frame(&mut stream).await?;
    if msg_type != MsgType::HelloAck {
        return Err(format!("Expected HELLO_ACK frame, got {:?}", msg_type).into());
    }
    let ack: HelloAckPayload = decode_json(&payload)?;
    if !ack.ok {
        let err = ack
            .error
            .unwrap_or_else(|| "Handshake rejected by agent".into());
        return Err(err.into());
    }

    let remote_workdir = ack
        .remote_workdir
        .ok_or("Agent did not provide remote workspace directory in HELLO_ACK")?;
    let compression = ack.compression.unwrap_or_else(|| "gzip".into());

    let local_uri_prefix = path_to_file_uri(local_dir);
    let remote_uri_prefix = path_to_file_uri(Path::new(&remote_workdir));

    // 1. Initial delta sync
    if !no_sync {
        sync_project(&mut stream, local_dir, &compression).await?;
    }

    // 2. Start remote LSP server with raw_stdio: true
    let run = RunPayload {
        argv: lsp_command.to_vec(),
        outputs: None,
        cwd: None,
        template: None,
        no_cache: true,
        env: None,
        tty: false,
        cols: None,
        rows: None,
        toolchain: None,
        raw_stdio: Some(true),
    };
    write_json_frame(&mut stream, MsgType::Run, &run).await?;

    let (mut net_read, net_write) = tokio::io::split(stream);
    let shared_net_write = Arc::new(Mutex::new(net_write));

    // 3. Task: Forward agent stdout/stderr to local stdout/stderr with remote -> local URI translation
    let local_prefix_clone = local_uri_prefix.clone();
    let remote_prefix_clone = remote_uri_prefix.clone();
    let stdout_task = tokio::spawn(async move {
        let mut parser = LspStreamParser::new();
        let mut std_out = tokio::io::stdout();
        let mut std_err = tokio::io::stderr();

        loop {
            match read_frame(&mut net_read).await {
                Ok((MsgType::Log, payload)) => {
                    if let Ok(log) = serde_json::from_slice::<LogPayload>(&payload) {
                        if log.stream == "stderr" {
                            let _ = std_err.write_all(log.data.as_bytes()).await;
                            let _ = std_err.flush().await;
                        } else {
                            parser.feed(log.data.as_bytes());
                            while let Some(msg) = parser.next_message() {
                                let translated = translate_uri_string(
                                    &msg,
                                    &remote_prefix_clone,
                                    &local_prefix_clone,
                                );
                                let encoded = encode_lsp_message(&translated);
                                let _ = std_out.write_all(&encoded).await;
                                let _ = std_out.flush().await;
                            }
                        }
                    }
                }
                Ok((MsgType::Result, payload)) => {
                    if let Ok(res) = serde_json::from_slice::<ResultPayload>(&payload) {
                        return res.exit_code;
                    }
                    return 0;
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        0
    });

    // 4. Task: Read from local stdin, translate local -> remote URIs, send Stdin frame
    let writer_clone = shared_net_write.clone();
    let host_str = host.to_string();
    let token_str = token.to_string();
    let proj_str = project.to_string();
    let local_dir_buf = local_dir.to_path_buf();
    let tls_clone = tls_config.cloned();

    let stdin_task = tokio::spawn(async move {
        let mut std_in = tokio::io::stdin();
        let mut parser = LspStreamParser::new();
        let mut buf = [0u8; 8192];

        loop {
            match std_in.read(&mut buf).await {
                Ok(0) => break, // Stdin closed
                Ok(n) => {
                    parser.feed(&buf[..n]);
                    while let Some(msg) = parser.next_message() {
                        // Check for textDocument/didSave
                        if !no_save_sync && msg.contains("\"textDocument/didSave\"") {
                            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&msg) {
                                if let Some(uri) = v
                                    .pointer("/params/textDocument/uri")
                                    .and_then(|u| u.as_str())
                                {
                                    if uri.starts_with(&local_uri_prefix) {
                                        let rel =
                                            uri[local_uri_prefix.len()..].trim_start_matches('/');
                                        let h = host_str.clone();
                                        let t = token_str.clone();
                                        let p = proj_str.clone();
                                        let ld = local_dir_buf.clone();
                                        let r = rel.to_string();
                                        let tc = tls_clone.clone();
                                        tokio::spawn(async move {
                                            let _ =
                                                sync_single_file(&h, &t, &p, &ld, &r, tc.as_ref())
                                                    .await;
                                        });
                                    }
                                }
                            }
                        }

                        let translated =
                            translate_uri_string(&msg, &local_uri_prefix, &remote_uri_prefix);
                        let encoded = encode_lsp_message(&translated);
                        let mut w = writer_clone.lock().await;
                        if write_frame(&mut *w, MsgType::Stdin, &encoded)
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                }
                Err(_) => break,
            }
        }
    });

    tokio::select! {
        code = stdout_task => {
            code.unwrap_or(0)
        }
        _ = stdin_task => {
            0
        }
    };

    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lsp_stream_parser_and_translation() {
        let mut parser = LspStreamParser::new();
        let raw_json = r#"{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":"file:///remote/workspace/src/lib.rs"}}"#;
        let encoded = encode_lsp_message(raw_json);

        // Feed partial bytes
        parser.feed(&encoded[..20]);
        assert_eq!(parser.next_message(), None);

        // Feed remainder
        parser.feed(&encoded[20..]);
        let msg = parser.next_message().expect("should extract message");
        assert_eq!(msg, raw_json);

        let translated = translate_uri_string(
            &msg,
            "file:///remote/workspace",
            "file:///home/user/project",
        );
        assert!(translated.contains("file:///home/user/project/src/lib.rs"));
    }

    #[test]
    fn test_path_to_file_uri() {
        let p = Path::new("/tmp/my-project");
        let uri = path_to_file_uri(p);
        assert_eq!(uri, "file:///tmp/my-project");
    }
}
