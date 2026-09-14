use protocol::RunRecord;
use std::fs;
use std::io;
use std::path::Path;
use std::time::SystemTime;

pub const RUNS_DIR: &str = ".farhand-runs";
pub const MAX_RUNS_RETAINED: usize = 100;

/// Format a `SystemTime` as an ISO-8601 / RFC-3339 UTC timestamp string.
pub fn format_rfc3339(time: SystemTime) -> String {
    let dur = match time.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => d,
        Err(_) => return "1970-01-01T00:00:00Z".to_string(),
    };
    let total_secs = dur.as_secs();
    let sec = (total_secs % 60) as u32;
    let total_mins = total_secs / 60;
    let min = (total_mins % 60) as u32;
    let total_hours = total_mins / 60;
    let hour = (total_hours % 24) as u32;
    let mut days = (total_hours / 24) as i64;

    // Howard Hinnant's algorithm for converting days since 1970-01-01 to Gregorian Y/M/D
    days += 719468;
    let era = if days >= 0 { days } else { days - 146096 } / 146097;
    let doe = (days - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y, m, d, hour, min, sec
    )
}

/// Persist a completed run record to `<workspace_dir>/.farhand-runs/`
/// and prune records exceeding the 100-run retention cap.
pub fn save_run(workspace_dir: &Path, record: &RunRecord) -> io::Result<()> {
    let runs_dir = workspace_dir.join(RUNS_DIR);
    fs::create_dir_all(&runs_dir)?;

    let now_millis = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);

    // Sanitize ID for filename safety
    let clean_id: String = record
        .id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();

    let filename = format!("{:020}_{}.json", now_millis, clean_id);
    let file_path = runs_dir.join(filename);

    let json = serde_json::to_string_pretty(record)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    fs::write(&file_path, json)?;

    // Prune records older than MAX_RUNS_RETAINED
    prune_old_runs(&runs_dir, MAX_RUNS_RETAINED)?;

    Ok(())
}

fn prune_old_runs(runs_dir: &Path, max_runs: usize) -> io::Result<()> {
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(runs_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("json") {
                if let Some(file_name) = path.file_name().and_then(|s| s.to_str()) {
                    files.push((file_name.to_string(), path));
                }
            }
        }
    }

    if files.len() > max_runs {
        // Filenames are prefixed with zero-padded timestamp millis, so ascending sort gives oldest first
        files.sort_by(|a, b| a.0.cmp(&b.0));
        let to_remove = files.len() - max_runs;
        for (_, path) in files.into_iter().take(to_remove) {
            let _ = fs::remove_file(path);
        }
    }

    Ok(())
}

/// Retrieve the most recent run records from `<workspace_dir>/.farhand-runs/`, sorted newest first.
pub fn get_recent_runs(workspace_dir: &Path, limit: usize) -> io::Result<Vec<RunRecord>> {
    let runs_dir = workspace_dir.join(RUNS_DIR);
    if !runs_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(&runs_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("json") {
                if let Some(file_name) = path.file_name().and_then(|s| s.to_str()) {
                    files.push((file_name.to_string(), path));
                }
            }
        }
    }

    // Sort descending by filename prefix (newest first)
    files.sort_by(|a, b| b.0.cmp(&a.0));

    let mut records = Vec::new();
    for (_, path) in files.into_iter().take(limit) {
        if let Ok(content) = fs::read_to_string(&path) {
            if let Ok(record) = serde_json::from_str::<RunRecord>(&content) {
                records.push(record);
            }
        }
    }

    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_format_rfc3339_epoch_and_recent() {
        let epoch = SystemTime::UNIX_EPOCH;
        assert_eq!(format_rfc3339(epoch), "1970-01-01T00:00:00Z");

        // 2026-09-14 10:45:00 UTC = 1789382700 seconds
        let fixed_time = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1789382700);
        assert_eq!(format_rfc3339(fixed_time), "2026-09-14T10:45:00Z");
    }

    #[test]
    fn test_save_and_get_recent_runs() {
        let dir = tempdir().unwrap();
        let ws_dir = dir.path();

        let run1 = RunRecord {
            id: "run-001".to_string(),
            timestamp_rfc3339: "2026-09-14T10:00:00Z".to_string(),
            project: "test-app".to_string(),
            argv: vec!["cargo".to_string(), "build".to_string()],
            exit_code: 0,
            duration_ms: 1200,
            bytes_synced: 1024,
            artifact_size: 4096,
            client_addr: "127.0.0.1:54321".to_string(),
            error: None,
        };

        save_run(ws_dir, &run1).unwrap();

        // Short sleep to ensure different millisecond filename
        std::thread::sleep(std::time::Duration::from_millis(5));

        let run2 = RunRecord {
            id: "run-002".to_string(),
            timestamp_rfc3339: "2026-09-14T10:05:00Z".to_string(),
            project: "test-app".to_string(),
            argv: vec!["cargo".to_string(), "test".to_string()],
            exit_code: 1,
            duration_ms: 800,
            bytes_synced: 0,
            artifact_size: 0,
            client_addr: "127.0.0.1:54322".to_string(),
            error: Some("test failure".to_string()),
        };

        save_run(ws_dir, &run2).unwrap();

        let runs = get_recent_runs(ws_dir, 10).unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].id, "run-002");
        assert_eq!(runs[1].id, "run-001");

        // Test limit
        let limited = get_recent_runs(ws_dir, 1).unwrap();
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].id, "run-002");
    }

    #[test]
    fn test_prune_old_runs_retention_cap() {
        let dir = tempdir().unwrap();
        let ws_dir = dir.path();

        for i in 0..105 {
            let run = RunRecord {
                id: format!("run-{:03}", i),
                timestamp_rfc3339: format!("2026-09-14T10:{:02}:00Z", i % 60),
                project: "test-app".to_string(),
                argv: vec!["echo".to_string(), i.to_string()],
                exit_code: 0,
                duration_ms: 10,
                bytes_synced: 0,
                artifact_size: 0,
                client_addr: "127.0.0.1:12345".to_string(),
                error: None,
            };
            save_run(ws_dir, &run).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }

        let runs_dir = ws_dir.join(RUNS_DIR);
        let count = fs::read_dir(&runs_dir).unwrap().count();
        assert_eq!(count, 100);

        let recent = get_recent_runs(ws_dir, 200).unwrap();
        assert_eq!(recent.len(), 100);
        // Newest should be run-104
        assert_eq!(recent[0].id, "run-104");
    }
}
