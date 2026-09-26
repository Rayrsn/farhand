//! `fh sync` and `fh why`: explain and control what crosses the wire.
//!
//! A build reports its sync only as a side effect. These commands make the
//! transfer itself the subject: `sync --dry-run` answers "what would you send
//! and why" without sending a byte, and `why` answers the same question for a
//! single path. Both are built on the ordinary manifest/NEED exchange, so the
//! answer comes from the agent's real state rather than a local guess.

use std::path::Path;

use protocol::{
    decode_json, read_frame, write_frame, write_json_frame, FileEntry, HelloPayload,
    ManifestPayload, MsgType, NeedPayload, CURRENT_PROTOCOL_VERSION,
};

use crate::client::connect_to_agent;

/// What one sync pass found, ready to be rendered or asserted on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncReport {
    /// Files the local scan kept.
    pub files_scanned: usize,
    /// Total size of the kept files.
    pub bytes_scanned: u64,
    /// Files the agent asked for (changed or missing remotely).
    pub files_to_transfer: usize,
    /// Uncompressed size of those files.
    pub bytes_to_transfer: u64,
    /// Files the agent already has byte-identical (content-addressed hit).
    pub files_already_present: usize,
    /// Their combined size — bytes that will *not* cross the network.
    pub bytes_already_present: u64,
    /// Bytes actually uploaded (compressed); 0 for a dry run.
    pub bytes_uploaded: u64,
    /// The exact paths the agent asked for. Aggregates above are for display;
    /// a per-path question ("is *this* file going to move?") needs the set.
    pub files_wanted: Vec<String>,
    /// Whether the agent was asked to queue behind a lock.
    pub was_queued: bool,
}

impl SyncReport {
    /// The headline a user cares about: how much of the project moves.
    pub fn transfer_ratio_percent(&self) -> u64 {
        if self.bytes_scanned == 0 {
            return 0;
        }
        (self.bytes_to_transfer * 100) / self.bytes_scanned
    }
}

/// The fate of a single path, as `fh why` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WhyOutcome {
    /// The path is not part of the project at all (outside the project dir).
    OutsideProject,
    /// Excluded by an ignore rule; carries a human explanation when we have one.
    Ignored { reason: Option<String> },
    /// Will be sent because the agent does not have this exact content.
    WillUpload { size: u64, hash: String },
    /// The agent already holds this exact content — a content-addressed hit.
    AlreadyPresent { size: u64, hash: String },
}

/// A sync-only session: connect, exchange a manifest, optionally upload.
pub struct SyncRequest<'a> {
    pub host: &'a str,
    pub token: &'a str,
    pub project_name: &'a str,
    pub project_dir: &'a Path,
    /// Report what would move without sending file data.
    pub dry_run: bool,
    /// Compression to pack with when actually uploading.
    pub compression: Option<String>,
    pub tls_config: Option<&'a config::TlsConfig>,
}

/// Explain what a sync of this project would do, and (unless `dry_run`) do it.
pub async fn sync_once(
    req: SyncRequest<'_>,
) -> Result<SyncReport, Box<dyn std::error::Error + Send + Sync>> {
    let scanned = fileset::scan(req.project_dir, &[])
        .map_err(|e| format!("failed to scan project files: {e}"))?;

    let manifest = ManifestPayload {
        files: scanned
            .values()
            .map(|meta| FileEntry {
                path: meta.path.clone(),
                hash: meta.hash.clone(),
                size: meta.size,
                mode: meta.mode,
            })
            .collect(),
    };

    let mut stream = connect_to_agent(req.host, req.tls_config).await?;

    let hello = HelloPayload {
        token: req.token.to_string(),
        project: req.project_name.to_string(),
        protocol_version: CURRENT_PROTOCOL_VERSION,
        compressions: req.compression.clone().map(|c| vec![c]),
    };
    write_json_frame(&mut stream, MsgType::Hello, &hello).await?;

    // Read HELLO_ACK, tolerating the agent telling us to wait for a lock.
    let mut was_queued = false;
    let ack = loop {
        let (msg_type, payload) = read_frame(&mut stream).await?;
        match msg_type {
            MsgType::Queued => {
                was_queued = true;
            }
            MsgType::HelloAck => break decode_json::<protocol::HelloAckPayload>(&payload)?,
            other => return Err(format!("expected HELLO_ACK, received {other:?}").into()),
        }
    };
    if !ack.ok {
        return Err(format!(
            "agent rejected the connection: {}",
            ack.error.as_deref().unwrap_or("unauthorized")
        )
        .into());
    }

    let negotiation = ack
        .compression
        .clone()
        .unwrap_or_else(|| "gzip".to_string());
    let compression_algo = fileset::CompressionAlgo::from_str_opt(Some(&negotiation));

    write_json_frame(&mut stream, MsgType::Manifest, &manifest).await?;

    // Read NEED, tolerating QUEUED while the agent waits on a lock.
    let need: NeedPayload = loop {
        let (msg_type, payload) = read_frame(&mut stream).await?;
        match msg_type {
            MsgType::Queued => {
                was_queued = true;
            }
            MsgType::Need => break decode_json(&payload)?,
            other => return Err(format!("expected NEED, received {other:?}").into()),
        }
    };

    let bytes_scanned: u64 = manifest.files.iter().map(|f| f.size).sum();
    let bytes_to_transfer: u64 = need
        .want
        .iter()
        .filter_map(|p| manifest.files.iter().find(|f| &f.path == p))
        .map(|f| f.size)
        .sum();
    let files_already_present = manifest.files.len().saturating_sub(need.want.len());
    let bytes_already_present = bytes_scanned.saturating_sub(bytes_to_transfer);

    let mut report = SyncReport {
        files_scanned: manifest.files.len(),
        bytes_scanned,
        files_to_transfer: need.want.len(),
        bytes_to_transfer,
        files_already_present,
        bytes_already_present,
        bytes_uploaded: 0,
        files_wanted: need.want.clone(),
        was_queued,
    };

    if req.dry_run {
        // Nothing leaves the machine: no FILES frame, no session to finish.
        return Ok(report);
    }

    let tar = if need.want.is_empty() {
        Vec::new()
    } else {
        fileset::pack_tar_with_algo(req.project_dir, &need.want, compression_algo)
            .map_err(|e| format!("failed to pack delta files: {e}"))?
    };
    report.bytes_uploaded = tar.len() as u64;
    write_frame(&mut stream, MsgType::Files, &tar).await?;

    // A sync-only session ends here: drop the connection instead of sending a
    // RUN frame. The agent treats that as "workspace updated" (see fhd).
    drop(stream);
    Ok(report)
}

/// Resolve `raw` against the project directory and explain what happens to it.
pub async fn why(
    req: SyncRequest<'_>,
    raw: &str,
) -> Result<WhyOutcome, Box<dyn std::error::Error + Send + Sync>> {
    let wanted = normalize_query(req.project_dir, raw);

    let scanned = fileset::scan(req.project_dir, &[])
        .map_err(|e| format!("failed to scan project files: {e}"))?;

    let Some(meta) = scanned.get(&wanted) else {
        // Not in the manifest: either outside the project or ignored. The
        // ignore engine can tell us which, and why.
        return Ok(
            match fileset::explain_ignore(req.project_dir, &wanted, &[]) {
                Some(reason) => WhyOutcome::Ignored {
                    reason: Some(reason),
                },
                None => WhyOutcome::OutsideProject,
            },
        );
    };

    let report = sync_once(SyncRequest {
        dry_run: true,
        ..req
    })
    .await?;

    if report.files_wanted.iter().any(|p| p == &wanted) {
        Ok(WhyOutcome::WillUpload {
            size: meta.size,
            hash: meta.hash.clone(),
        })
    } else {
        Ok(WhyOutcome::AlreadyPresent {
            size: meta.size,
            hash: meta.hash.clone(),
        })
    }
}

/// Turn a user-supplied path into the wire form the manifest uses.
fn normalize_query(project_dir: &Path, raw: &str) -> String {
    let candidate = std::path::Path::new(raw);
    let absolute = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        project_dir.join(candidate)
    };
    match absolute.strip_prefix(project_dir) {
        Ok(rel) => protocol::to_wire_path(rel),
        Err(_) => protocol::to_wire_path(&absolute),
    }
}

/// Human-readable size, used by the renderer in `main`.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
