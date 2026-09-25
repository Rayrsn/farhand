use config::TlsConfig;
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    style::{Color, Print, ResetColor, SetForegroundColor},
    terminal::{
        disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
};
use protocol::StatusResponsePayload;
use std::io::{stdout, Write};
use std::time::{Duration, Instant};

/// Formats uptime in seconds to a human-readable string.
pub fn format_uptime(secs: u64) -> String {
    let days = secs / 86400;
    let hours = (secs % 86400) / 3600;
    let mins = (secs % 3600) / 60;
    let s = secs % 60;

    if days > 0 {
        format!("{}d {}h {}m", days, hours, mins)
    } else if hours > 0 {
        format!("{}h {}m {}s", hours, mins, s)
    } else if mins > 0 {
        format!("{}m {}s", mins, s)
    } else {
        format!("{}s", s)
    }
}

/// Computes usage percentage and an ASCII bar gauge.
pub fn format_bar(used: u64, total: u64, width: usize) -> (f64, String) {
    if total == 0 {
        return (0.0, format!("[{}]", ".".repeat(width)));
    }
    let pct = (used as f64 / total as f64).clamp(0.0, 1.0);
    let filled = ((pct * width as f64).round() as usize).min(width);
    let empty = width.saturating_sub(filled);
    (
        pct * 100.0,
        format!("[{}{}]", "|".repeat(filled), ".".repeat(empty)),
    )
}

/// Renders a formatted snapshot of agent status to standard output.
pub fn render_snapshot(status: &StatusResponsePayload, host: &str) {
    println!("=== Farhand Agent: {} ({}) ===", status.hostname, host);

    let uptime_str = status
        .uptime_secs
        .map(format_uptime)
        .unwrap_or_else(|| "unknown".into());
    let cores_str = status
        .cpu_count
        .map(|c| c.to_string())
        .unwrap_or_else(|| "unknown".into());
    let load_str = match status.load_averages {
        Some([l1, l5, l15]) => format!("{:.2}, {:.2}, {:.2}", l1, l5, l15),
        None => "N/A".into(),
    };
    println!(
        "Uptime: {:<12} | Cores: {:<4} | Load Averages: {}",
        uptime_str, cores_str, load_str
    );

    if let (Some(used), Some(total)) = (status.memory_used_bytes, status.memory_total_bytes) {
        let (pct, bar) = format_bar(used, total, 20);
        println!(
            "Memory: {:>7} / {:<7} ({:>5.1}%) {}",
            crate::history::format_bytes(used),
            crate::history::format_bytes(total),
            pct,
            bar
        );
    }

    if let (Some(free), Some(total)) = (status.disk_free_bytes, status.disk_total_bytes) {
        let used = total.saturating_sub(free);
        let (pct, bar) = format_bar(used, total, 20);
        println!(
            "Disk:   {:>7} / {:<7} ({:>5.1}%) {}",
            crate::history::format_bytes(used),
            crate::history::format_bytes(total),
            pct,
            bar
        );
    }

    let tags_str = if status.tags.is_empty() {
        "none".to_string()
    } else {
        status.tags.join(", ")
    };
    let ws_str = status
        .workspaces_count
        .map(|w| w.to_string())
        .unwrap_or_else(|| "N/A".into());
    println!(
        "Runs:   {}/{} active (Queue: {}) | Workspaces: {} | Tags: [{}]",
        status.active_runs, status.max_runs, status.queue_depth, ws_str, tags_str
    );
    println!();

    let builds = status.active_builds.as_deref().unwrap_or(&[]);
    if builds.is_empty() {
        println!("ACTIVE BUILDS: None (agent idle)");
    } else {
        println!("ACTIVE BUILDS ({}):", builds.len());
        println!(
            "{:<10} {:<18} {:<10} {:<22} COMMAND",
            "ID", "PROJECT", "ELAPSED", "CLIENT"
        );
        println!(
            "{:-<10} {:-<18} {:-<10} {:-<22} {:-<20}",
            "-", "-", "-", "-", "-"
        );
        for b in builds {
            let cmd = b.argv.join(" ");
            let cmd_trunc = if cmd.len() > 40 {
                format!("{}...", crate::truncate_utf8(&cmd, 37))
            } else {
                cmd
            };
            println!(
                "{:<10} {:<18} {:<10} {:<22} {}",
                b.id,
                b.project,
                crate::history::format_duration(b.elapsed_ms),
                b.client_addr,
                cmd_trunc
            );
        }
    }
}

/// Queries agent info and prints either structured JSON or human-readable format.
pub async fn run_agent_info(
    host: &str,
    token: &str,
    tls_config: Option<&TlsConfig>,
    as_json: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let status = crate::pool::probe_agent_status(host, token, tls_config).await?;
    if as_json {
        let json_str = serde_json::to_string_pretty(&status)?;
        println!("{}", json_str);
    } else {
        render_snapshot(&status, host);
    }
    Ok(())
}

struct TerminalRestorer;
impl Drop for TerminalRestorer {
    fn drop(&mut self) {
        let mut out = stdout();
        let _ = execute!(out, LeaveAlternateScreen, cursor::Show);
        let _ = disable_raw_mode();
    }
}

/// Runs the interactive live terminal dashboard loop.
pub async fn run_top(
    host: &str,
    token: &str,
    tls_config: Option<&TlsConfig>,
    once: bool,
    interval_secs: u64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if once {
        let status = crate::pool::probe_agent_status(host, token, tls_config).await?;
        render_snapshot(&status, host);
        return Ok(());
    }

    // Set up TUI terminal mode
    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen, cursor::Hide)?;
    let _restorer = TerminalRestorer;

    let refresh_interval = Duration::from_secs(interval_secs.max(1));
    let mut last_fetch = Instant::now() - refresh_interval;
    let mut current_status: Option<Result<StatusResponsePayload, String>> = None;

    loop {
        if last_fetch.elapsed() >= refresh_interval || current_status.is_none() {
            let res = crate::pool::probe_agent_status(host, token, tls_config)
                .await
                .map_err(|e| e.to_string());
            current_status = Some(res);
            last_fetch = Instant::now();
        }

        // Draw screen
        execute!(out, cursor::MoveTo(0, 0), Clear(ClearType::All))?;

        execute!(
            out,
            SetForegroundColor(Color::Cyan),
            Print(format!("=== Farhand Agent Dashboard: {} ===\r\n", host)),
            ResetColor
        )?;

        match &current_status {
            Some(Ok(status)) => {
                let uptime_str = status
                    .uptime_secs
                    .map(format_uptime)
                    .unwrap_or_else(|| "unknown".into());
                let cores_str = status
                    .cpu_count
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "unknown".into());
                let load_str = match status.load_averages {
                    Some([l1, l5, l15]) => format!("{:.2}, {:.2}, {:.2}", l1, l5, l15),
                    None => "N/A".into(),
                };

                execute!(
                    out,
                    Print(format!(
                        " Hostname: {:<16} Uptime: {:<12} Cores: {:<4}\r\n",
                        status.hostname, uptime_str, cores_str
                    )),
                    Print(format!(" Load Averages: {}\r\n", load_str)),
                )?;

                if let (Some(used), Some(total)) =
                    (status.memory_used_bytes, status.memory_total_bytes)
                {
                    let (pct, bar) = format_bar(used, total, 24);
                    let color = if pct > 85.0 {
                        Color::Red
                    } else if pct > 65.0 {
                        Color::Yellow
                    } else {
                        Color::Green
                    };
                    execute!(
                        out,
                        Print(" Memory: "),
                        SetForegroundColor(color),
                        Print(format!(
                            "{:>7} / {:<7} ({:>5.1}%) {}\r\n",
                            crate::history::format_bytes(used),
                            crate::history::format_bytes(total),
                            pct,
                            bar
                        )),
                        ResetColor,
                    )?;
                }

                if let (Some(free), Some(total)) = (status.disk_free_bytes, status.disk_total_bytes)
                {
                    let used = total.saturating_sub(free);
                    let (pct, bar) = format_bar(used, total, 24);
                    let color = if pct > 85.0 {
                        Color::Red
                    } else if pct > 65.0 {
                        Color::Yellow
                    } else {
                        Color::Green
                    };
                    execute!(
                        out,
                        Print(" Disk:   "),
                        SetForegroundColor(color),
                        Print(format!(
                            "{:>7} / {:<7} ({:>5.1}%) {}\r\n",
                            crate::history::format_bytes(used),
                            crate::history::format_bytes(total),
                            pct,
                            bar
                        )),
                        ResetColor,
                    )?;
                }

                let ws_str = status
                    .workspaces_count
                    .map(|w| w.to_string())
                    .unwrap_or_else(|| "N/A".into());
                let tags_str = if status.tags.is_empty() {
                    "none".to_string()
                } else {
                    status.tags.join(", ")
                };
                execute!(
                    out,
                    Print(format!(
                        " Runs:   {}/{} active (Queue: {}) | Workspaces: {} | Tags: [{}]\r\n\r\n",
                        status.active_runs, status.max_runs, status.queue_depth, ws_str, tags_str
                    )),
                )?;

                let builds = status.active_builds.as_deref().unwrap_or(&[]);
                if builds.is_empty() {
                    execute!(
                        out,
                        SetForegroundColor(Color::DarkGrey),
                        Print(" Active Builds: None (agent idle)\r\n"),
                        ResetColor,
                    )?;
                } else {
                    execute!(
                        out,
                        SetForegroundColor(Color::Yellow),
                        Print(format!(" Active Builds ({}):\r\n", builds.len())),
                        ResetColor,
                        Print(format!(
                            " {:<10} {:<16} {:<10} {:<20} COMMAND\r\n",
                            "ID", "PROJECT", "ELAPSED", "CLIENT"
                        )),
                        Print(format!(
                            " {:-<10} {:-<16} {:-<10} {:-<20} {:-<25}\r\n",
                            "-", "-", "-", "-", "-"
                        )),
                    )?;
                    for b in builds {
                        let cmd = b.argv.join(" ");
                        let cmd_trunc = if cmd.len() > 35 {
                            format!("{}...", crate::truncate_utf8(&cmd, 32))
                        } else {
                            cmd
                        };
                        execute!(
                            out,
                            Print(format!(
                                " {:<10} {:<16} {:<10} {:<20} {}\r\n",
                                b.id,
                                b.project,
                                crate::history::format_duration(b.elapsed_ms),
                                b.client_addr,
                                cmd_trunc
                            )),
                        )?;
                    }
                }
            }
            Some(Err(err)) => {
                execute!(
                    out,
                    SetForegroundColor(Color::Red),
                    Print(format!(" Error querying agent status: {}\r\n", err)),
                    ResetColor,
                )?;
            }
            None => {}
        }

        execute!(
            out,
            Print("\r\n"),
            SetForegroundColor(Color::DarkGrey),
            Print(format!(
                " [q/Esc] Quit | Refresh interval: {}s | Last updated: {:?}\r\n",
                interval_secs,
                last_fetch.elapsed()
            )),
            ResetColor,
        )?;
        out.flush()?;

        // Wait up to 250ms for user input
        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => break,
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                    _ => {}
                }
            }
        }
    }

    Ok(())
}
