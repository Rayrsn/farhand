use protocol::{
    decode_json, read_frame, write_json_frame, HelloAckPayload, HistoryRequestPayload,
    HistoryResponsePayload, MsgType,
};

/// Truncate a string to at most `max_bytes` without splitting UTF-8
/// characters. Byte-slicing (`&s[..37]`) panics on multibyte boundaries;
/// this helper always lands on a char boundary.
pub fn truncate_utf8(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Queries execution and build history for a project from a remote agent daemon.
pub async fn query_history(
    host: &str,
    token: &str,
    project: &str,
    limit: usize,
    tls_config: Option<&config::TlsConfig>,
) -> Result<HistoryResponsePayload, Box<dyn std::error::Error + Send + Sync>> {
    let mut stream = crate::connect_to_agent(host, tls_config).await?;
    let req = HistoryRequestPayload {
        token: token.to_string(),
        project: project.to_string(),
        limit,
    };
    write_json_frame(&mut stream, MsgType::History, &req).await?;

    let (msg_type, payload) = read_frame(&mut stream).await?;
    if msg_type == MsgType::HelloAck {
        let ack: HelloAckPayload = decode_json(&payload)?;
        return Err(ack
            .error
            .unwrap_or_else(|| "Unauthorized history request".into())
            .into());
    }
    if msg_type != MsgType::HistoryResp {
        return Err(format!("Expected HISTORY_RESP frame, got {:?}", msg_type).into());
    }

    let resp: HistoryResponsePayload = decode_json(&payload)?;
    Ok(resp)
}

pub fn format_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{}ms", ms)
    } else {
        format!("{:.2}s", (ms as f64) / 1000.0)
    }
}

pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    if bytes >= GB {
        format!("{:.1} GB", (bytes as f64) / (GB as f64))
    } else if bytes >= MB {
        format!("{:.1} MB", (bytes as f64) / (MB as f64))
    } else if bytes >= KB {
        format!("{:.1} KB", (bytes as f64) / (KB as f64))
    } else {
        format!("{} B", bytes)
    }
}

pub fn render_history_table(resp: &HistoryResponsePayload) {
    if resp.runs.is_empty() {
        println!("No execution history found for project '{}'.", resp.project);
        return;
    }

    println!(
        "{:<10} {:<22} {:<6} {:<10} {:<10} {:<10} COMMAND",
        "ID", "DATE / TIME (UTC)", "EXIT", "DURATION", "SYNCED", "ARTIFACTS"
    );
    println!(
        "{:-<10} {:-<22} {:-<6} {:-<10} {:-<10} {:-<10} {:-<20}",
        "-", "-", "-", "-", "-", "-", "-"
    );

    for run in &resp.runs {
        let cmd = run.argv.join(" ");
        let cmd_truncated = if cmd.len() > 40 {
            format!("{}...", crate::truncate_utf8(&cmd, 37))
        } else {
            cmd
        };
        println!(
            "{:<10} {:<22} {:<6} {:<10} {:<10} {:<10} {}",
            run.id,
            run.timestamp_rfc3339,
            run.exit_code,
            format_duration(run.duration_ms),
            format_bytes(run.bytes_synced),
            format_bytes(run.artifact_size),
            cmd_truncated,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_utf8_never_splits_multibyte_characters() {
        // Regression: `&cmd[..37]` panicked when byte 37 landed inside a
        // multibyte character (e.g. CJK commands).
        let cjk = "编译项目编译项目编译项目编译项目编译项目编译项目"; // 3-byte chars
        assert!(cjk.len() > 40);
        let t = truncate_utf8(cjk, 37);
        assert!(t.len() <= 37);
        assert!(std::str::from_utf8(t.as_bytes()).is_ok());

        let emoji = "build 🚀 project 🚀 build 🚀 build 🚀 build 🚀 build 🚀";
        assert!(emoji.len() > 40);
        let t = truncate_utf8(emoji, 37);
        assert!(t.is_char_boundary(t.len()));

        // Short strings pass through untouched.
        assert_eq!(truncate_utf8("short", 37), "short");
        // Exact boundary is kept.
        assert_eq!(truncate_utf8("abcdefgh", 4), "abcd");
    }
}
