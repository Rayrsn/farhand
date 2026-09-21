use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const CURRENT_PROTOCOL_VERSION: u32 = 1;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MsgType {
    Hello = 0x01,
    HelloAck = 0x02,
    Manifest = 0x03,
    Need = 0x04,
    Files = 0x05,
    Run = 0x06,
    Log = 0x07,
    Result = 0x08,
    Artifacts = 0x09,
    PutTemplate = 0x0A,
    Queued = 0x0B,
    Status = 0x0C,
    StatusResp = 0x0D,
    History = 0x0E,
    HistoryResp = 0x0F,
    Clean = 0x10,
    CleanResp = 0x11,
    Stdin = 0x12,
    Resize = 0x13,
    PortOpen = 0x14,
    PortData = 0x15,
    PortClose = 0x16,
}

impl MsgType {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0x01 => Some(Self::Hello),
            0x02 => Some(Self::HelloAck),
            0x03 => Some(Self::Manifest),
            0x04 => Some(Self::Need),
            0x05 => Some(Self::Files),
            0x06 => Some(Self::Run),
            0x07 => Some(Self::Log),
            0x08 => Some(Self::Result),
            0x09 => Some(Self::Artifacts),
            0x0A => Some(Self::PutTemplate),
            0x0B => Some(Self::Queued),
            0x0C => Some(Self::Status),
            0x0D => Some(Self::StatusResp),
            0x0E => Some(Self::History),
            0x0F => Some(Self::HistoryResp),
            0x10 => Some(Self::Clean),
            0x11 => Some(Self::CleanResp),
            0x12 => Some(Self::Stdin),
            0x13 => Some(Self::Resize),
            0x14 => Some(Self::PortOpen),
            0x15 => Some(Self::PortData),
            0x16 => Some(Self::PortClose),
            _ => None,
        }
    }

    pub fn to_u8(self) -> u8 {
        self as u8
    }
}

/// Client -> Agent: Initial authentication and protocol handshake
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HelloPayload {
    pub token: String,
    pub project: String,
    #[serde(rename = "protocolVersion")]
    pub protocol_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compressions: Option<Vec<String>>,
}

/// Agent -> Client: Handshake acknowledgement
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HelloAckPayload {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compression: Option<String>,
}

/// Metadata for an individual tracked file
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileEntry {
    /// Slash-separated relative path
    pub path: String,
    /// Hex-encoded SHA-256 digest
    pub hash: String,
    /// Size in bytes
    pub size: u64,
    /// POSIX file permission mode bits
    pub mode: u32,
}

/// Client -> Agent: Client fileset metadata for delta comparison
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestPayload {
    pub files: Vec<FileEntry>,
}

/// Agent -> Client: Files requested by agent and files to delete
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NeedPayload {
    pub want: Vec<String>,
    #[serde(rename = "deleteExtraneous", default)]
    pub delete_extraneous: Vec<String>,
}

/// Client -> Agent: Command execution request
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunPayload {
    pub argv: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outputs: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    #[serde(rename = "noCache", default)]
    pub no_cache: bool,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub env: Option<HashMap<String, String>>,
    #[serde(default)]
    pub tty: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cols: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows: Option<u16>,
}

/// Client -> Agent: Terminal window resize event
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResizePayload {
    pub cols: u16,
    pub rows: u16,
}

/// Client <-> Agent: Reverse port forwarding channel open request
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PortOpenPayload {
    #[serde(rename = "channelId")]
    pub channel_id: u32,
    #[serde(rename = "targetPort")]
    pub target_port: u16,
}

/// Client <-> Agent: Data chunk over a port forwarding channel
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PortDataPayload {
    #[serde(rename = "channelId")]
    pub channel_id: u32,
    pub data: Vec<u8>,
}

/// Client <-> Agent: Close a port forwarding channel
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PortClosePayload {
    #[serde(rename = "channelId")]
    pub channel_id: u32,
}

/// Agent -> Client: Streamed stdout or stderr line/chunk
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogPayload {
    pub stream: String, // "stdout" | "stderr"
    pub data: String,
}

/// Agent -> Client: Final process exit code and diagnostic error
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResultPayload {
    #[serde(rename = "exitCode")]
    pub exit_code: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Client -> Agent: Upload a custom template dynamically
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PutTemplatePayload {
    pub name: String,
    pub yaml: String,
    pub scope: String, // "user" | "project"
}

/// Agent -> Client: Information when a run is queued behind locks
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QueuedPayload {
    pub position: usize,
    pub reason: String, // "project_busy" | "concurrency_limit"
}

/// Client -> Agent: Health and queue status probe
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StatusRequestPayload {
    pub token: String,
}

/// Agent -> Client: Status response for multi-agent dispatch
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StatusResponsePayload {
    #[serde(rename = "activeRuns")]
    pub active_runs: usize,
    #[serde(rename = "maxRuns")]
    pub max_runs: usize,
    #[serde(rename = "queueDepth")]
    pub queue_depth: usize,
    pub hostname: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(
        rename = "diskFreeBytes",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub disk_free_bytes: Option<u64>,
    #[serde(
        rename = "diskTotalBytes",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub disk_total_bytes: Option<u64>,
}

/// Agent-side record of a completed run
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunRecord {
    pub id: String,
    pub timestamp_rfc3339: String,
    pub project: String,
    pub argv: Vec<String>,
    #[serde(rename = "exitCode")]
    pub exit_code: i32,
    #[serde(rename = "durationMs")]
    pub duration_ms: u64,
    #[serde(rename = "bytesSynced")]
    pub bytes_synced: u64,
    #[serde(rename = "artifactSize")]
    pub artifact_size: u64,
    #[serde(rename = "clientAddr")]
    pub client_addr: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Client -> Agent: History query
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryRequestPayload {
    pub token: String,
    pub project: String,
    #[serde(default = "default_history_limit")]
    pub limit: usize,
}

fn default_history_limit() -> usize {
    10
}

/// Agent -> Client: History reply
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryResponsePayload {
    pub project: String,
    pub runs: Vec<RunRecord>,
}

/// Client -> Agent: Request remote workspace cleaning / garbage collection
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CleanRequestPayload {
    pub token: String,
    pub project: String,
    #[serde(rename = "allBranches", default)]
    pub all_branches: bool,
    #[serde(rename = "cachesOnly", default)]
    pub caches_only: bool,
}

/// Agent -> Client: Result of workspace cleaning
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CleanResponsePayload {
    pub ok: bool,
    pub message: String,
    #[serde(rename = "bytesFreed")]
    pub bytes_freed: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_run_payload_serde_with_and_without_env() {
        let mut env = HashMap::new();
        env.insert("DATABASE_URL".to_string(), "postgres://...".to_string());
        env.insert("NODE_ENV".to_string(), "production".to_string());

        let payload_with_env = RunPayload {
            argv: vec!["npm".into(), "run".into(), "build".into()],
            outputs: Some(vec!["dist".into()]),
            cwd: None,
            template: Some("npm".into()),
            no_cache: false,
            env: Some(env.clone()),
            tty: false,
            cols: None,
            rows: None,
        };

        let json = serde_json::to_string(&payload_with_env).unwrap();
        let decoded: RunPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.env, Some(env));

        // Test without env (omitted in json, defaults to None)
        let json_without_env = r#"{"argv":["cargo","build"]}"#;
        let decoded2: RunPayload = serde_json::from_str(json_without_env).unwrap();
        assert_eq!(decoded2.env, None);
        assert!(!decoded2.no_cache);
        assert!(!decoded2.tty);
        assert_eq!(decoded2.cols, None);
        assert_eq!(decoded2.rows, None);
    }

    #[test]
    fn test_tier1_msg_types_and_payloads() {
        for val in 0x01..=0x16 {
            let msg = MsgType::from_u8(val).unwrap();
            assert_eq!(msg.to_u8(), val);
        }

        let resize = ResizePayload {
            cols: 120,
            rows: 40,
        };
        let json = serde_json::to_string(&resize).unwrap();
        let decoded: ResizePayload = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.cols, 120);
        assert_eq!(decoded.rows, 40);

        let port_open = PortOpenPayload {
            channel_id: 42,
            target_port: 3000,
        };
        let json = serde_json::to_string(&port_open).unwrap();
        let decoded: PortOpenPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.channel_id, 42);
        assert_eq!(decoded.target_port, 3000);

        let port_data = PortDataPayload {
            channel_id: 42,
            data: b"hello".to_vec(),
        };
        let json = serde_json::to_string(&port_data).unwrap();
        let decoded: PortDataPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.channel_id, 42);
        assert_eq!(decoded.data, b"hello");

        let port_close = PortClosePayload { channel_id: 42 };
        let json = serde_json::to_string(&port_close).unwrap();
        let decoded: PortClosePayload = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.channel_id, 42);
    }
}
