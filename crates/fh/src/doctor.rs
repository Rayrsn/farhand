//! `fh doctor`: answer "why isn't this working?" in one command.
//!
//! Every check here is read-only. Nothing is synced, nothing is executed on the
//! agent, and no run slot is consumed — the only remote traffic is the STATUS
//! probe, which is the same request `fh top` already makes. A doctor that
//! changed state to diagnose it would be a bad doctor.

use std::path::Path;
use std::time::Instant;

use protocol::StatusResponsePayload;

use crate::sync::human_bytes;

/// How bad a finding is. Only `Fail` is worth a non-zero exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Something the user probably wants to know but which still works.
    Warn,
    /// The setup is wrong; the thing they are trying to do will not work.
    Fail,
}

/// One line of the report.
#[derive(Debug, Clone)]
pub struct Check {
    pub name: &'static str,
    pub severity: Option<Severity>,
    pub detail: String,
    /// The concrete fix, when there is one. Worth more than the diagnosis.
    pub hint: Option<String>,
}

impl Check {
    fn ok(name: &'static str, detail: impl Into<String>) -> Self {
        Check {
            name,
            severity: None,
            detail: detail.into(),
            hint: None,
        }
    }

    fn warn(name: &'static str, detail: impl Into<String>, hint: impl Into<String>) -> Self {
        Check {
            name,
            severity: Some(Severity::Warn),
            detail: detail.into(),
            hint: Some(hint.into()),
        }
    }

    fn fail(name: &'static str, detail: impl Into<String>, hint: impl Into<String>) -> Self {
        Check {
            name,
            severity: Some(Severity::Fail),
            detail: detail.into(),
            hint: Some(hint.into()),
        }
    }
}

/// Everything the checks need, gathered once by `main` from the resolved config.
pub struct DoctorInput<'a> {
    pub host: &'a str,
    pub token: &'a str,
    /// Where the config came from, for the "what is in effect" line.
    pub config_path: Option<&'a Path>,
    pub project_name: &'a str,
    pub project_dir: &'a Path,
    /// Toolchains declared via `-T` or the config, already parsed.
    pub toolchains: &'a [(String, String)],
    pub tls: Option<&'a config::TlsConfig>,
    /// A raw token literal found in a config file, if any (plaintext = risky).
    pub plaintext_token_in_config: bool,
}

/// Run every check and return them in report order.
pub async fn run(input: DoctorInput<'_>) -> Vec<Check> {
    let mut checks = Vec::new();
    checks.extend(local_checks(&input));
    let probe = probe(&input).await;
    checks.extend(remote_checks(&input, &probe));
    checks
}

/// Everything answerable without touching the network.
fn local_checks(input: &DoctorInput<'_>) -> Vec<Check> {
    let mut checks = vec![];

    // Where is farhand actually pointed?
    checks.push(Check::ok(
        "Target",
        format!("{} (project {})", input.host, input.project_name),
    ));
    checks.push(Check::ok(
        "Project",
        input.project_dir.display().to_string(),
    ));

    // The single most common cause of "it just fails": no token.
    if input.token.is_empty() {
        checks.push(Check::fail(
            "Authentication",
            "no token configured",
            "set `token` in .farhand.yaml, pass --token, or export FARHAND_TOKEN",
        ));
    } else if input.plaintext_token_in_config {
        checks.push(Check::warn(
            "Authentication",
            "token is stored in plaintext in the config file",
            "use token: \"${FARHAND_TOKEN}\" and export the value, or `fh init --token-env`",
        ));
    } else {
        checks.push(Check::ok(
            "Authentication",
            "token present (value never printed)",
        ));
    }

    // A token over cleartext is the other common surprise.
    let tls_on = input.tls.map(|t| t.enabled).unwrap_or(false);
    if !tls_on && !input.token.is_empty() {
        checks.push(Check::warn(
            "Transport",
            "token will cross the network unencrypted",
            "enable TLS (--tls, or tls: true in config) for anything but loopback",
        ));
    } else if tls_on {
        let mode = if input.tls.is_some_and(|t| t.insecure) {
            "enabled, certificate verification DISABLED (--tls-insecure)"
        } else if input
            .tls
            .is_some_and(|t| t.fingerprint.is_some() || t.ca.is_some())
        {
            "enabled, verified by fingerprint or CA"
        } else {
            "enabled, trust-on-first-use"
        };
        let hint = input.tls.is_some_and(|t| t.insecure).then(|| {
            "drop --tls-insecure once the agent's certificate can be verified".to_string()
        });
        checks.push(Check {
            name: "Transport",
            severity: None,
            detail: format!("TLS {mode}"),
            hint,
        });
    } else {
        checks.push(Check::ok("Transport", "cleartext (no TLS configured)"));
    }

    // Declared toolchains. We cannot see the agent's toolchain from here
    // without executing something on it, so say exactly that rather than
    // implying we checked.
    if input.toolchains.is_empty() {
        checks.push(Check::ok(
            "Toolchains",
            "none declared; the agent's defaults are used",
        ));
    } else {
        let mut bad = Vec::new();
        let known = [
            "rust", "rustup", "python", "pyenv", "node", "nodejs", "go", "golang",
        ];
        for (lang, version) in input.toolchains {
            if version.trim().is_empty() {
                bad.push(format!("{lang} has no version"));
            } else if !known.contains(&lang.to_ascii_lowercase().as_str()) {
                bad.push(format!("{lang} is not a toolchain farhand manages"));
            }
        }
        let declared = input
            .toolchains
            .iter()
            .map(|(l, v)| format!("{l}={v}"))
            .collect::<Vec<_>>()
            .join(", ");
        if bad.is_empty() {
            checks.push(Check {
                name: "Toolchains",
                severity: None,
                detail: format!("declared {declared} (applied to the agent at run time)"),
                hint: None,
            });
        } else {
            checks.push(Check::warn(
                "Toolchains",
                format!("declared {declared}; {}", bad.join("; ")),
                "farhand manages rust, python, node, and go toolchains",
            ));
        }
    }

    checks
}

/// Ask the agent how it is doing, timing the round trip.
///
/// Deliberately two-phase. A single failed request cannot tell "the daemon is
/// not there" apart from "the daemon is there and rejected my token" — the
/// daemon just closes the connection in both cases — and those two problems
/// have completely different fixes. Connecting first is what lets the report
/// say which one it is.
async fn probe(input: &DoctorInput<'_>) -> Probe {
    use protocol::{decode_json, read_frame, write_json_frame, MsgType, StatusRequestPayload};

    let started = Instant::now();
    let mut stream = match tokio::time::timeout(
        std::time::Duration::from_secs(5),
        crate::connect_to_agent(input.host, input.tls),
    )
    .await
    {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            return Probe {
                status: None,
                outcome: ProbeOutcome::Unreachable(e.to_string()),
                elapsed: started.elapsed(),
            }
        }
        Err(_) => {
            return Probe {
                status: None,
                outcome: ProbeOutcome::Unreachable("connection timed out".to_string()),
                elapsed: started.elapsed(),
            }
        }
    };

    let request = StatusRequestPayload {
        token: input.token.to_string(),
    };
    if let Err(e) = write_json_frame(&mut stream, MsgType::Status, &request).await {
        return Probe {
            status: None,
            outcome: ProbeOutcome::Rejected(format!("could not send the status request: {e}")),
            elapsed: started.elapsed(),
        };
    }

    match read_frame(&mut stream).await {
        Ok((MsgType::StatusResp, payload)) => {
            match decode_json::<StatusResponsePayload>(&payload) {
                Ok(status) => Probe {
                    status: Some(status),
                    outcome: ProbeOutcome::Ok,
                    elapsed: started.elapsed(),
                },
                Err(e) => Probe {
                    status: None,
                    outcome: ProbeOutcome::Rejected(format!("unreadable status response: {e}")),
                    elapsed: started.elapsed(),
                },
            }
        }
        Ok((MsgType::HelloAck, _)) => {
            // The agent answered the request as a handshake and reported the
            // failure itself, which means it understood us and said no.
            Probe {
                status: None,
                outcome: ProbeOutcome::Rejected("the agent rejected the token".to_string()),
                elapsed: started.elapsed(),
            }
        }
        Ok((other, _)) => Probe {
            status: None,
            outcome: ProbeOutcome::Rejected(format!("unexpected frame {other:?}")),
            elapsed: started.elapsed(),
        },
        Err(e) => Probe {
            status: None,
            // The agent closes the connection without answering when the token
            // does not match — the most common misconfiguration by far.
            outcome: ProbeOutcome::Rejected(format!(
                "the agent closed the connection without answering (wrong token?): {e}"
            )),
            elapsed: started.elapsed(),
        },
    }
}

/// Why the probe did not produce a status.
#[derive(Debug)]
enum ProbeOutcome {
    Ok,
    /// Could not establish a connection at all.
    Unreachable(String),
    /// Connected, but the agent would not answer the request.
    Rejected(String),
}

/// The outcome of the STATUS probe, with how long it took.
struct Probe {
    status: Option<StatusResponsePayload>,
    outcome: ProbeOutcome,
    elapsed: std::time::Duration,
}

/// Everything that depends on the agent answering.
fn remote_checks(input: &DoctorInput<'_>, probe: &Probe) -> Vec<Check> {
    let mut checks = vec![];

    let Some(status) = &probe.status else {
        let (detail, hint) = match &probe.outcome {
            ProbeOutcome::Unreachable(why) => (
                format!("cannot reach {} ({why})", input.host),
                format!(
                    "is `fhd` running and listening on {}? check --listen, the port, \
                     and any firewall",
                    input.host
                ),
            ),
            ProbeOutcome::Rejected(why) => (
                format!("reached {} but it would not answer: {why}", input.host),
                "the token must match the agent's --token; check FARHAND_TOKEN and \
                 .farhand.yaml"
                    .to_string(),
            ),
            ProbeOutcome::Ok => unreachable!("ProbeOutcome::Ok always carries a status"),
        };
        checks.push(Check::fail("Connectivity", detail, hint));
        return checks;
    };

    checks.push(Check::ok(
        "Connectivity",
        format!(
            "agent {} answered in {} ms",
            status.hostname,
            probe.elapsed.as_millis()
        ),
    ));

    // Disk is the resource that silently fails builds.
    match (status.disk_free_bytes, status.disk_total_bytes) {
        (Some(free), Some(total)) => checks.push(Check::ok(
            "Disk",
            format!("{} free of {}", human_bytes(free), human_bytes(total)),
        )),
        (Some(free), None) => checks.push(Check::ok(
            "Disk",
            format!("{} free (total unknown)", human_bytes(free)),
        )),
        _ => checks.push(Check::warn(
            "Disk",
            "the agent did not report free space",
            "this platform's disk probe failed on the agent host",
        )),
    }

    // Capacity: a saturated agent is the other reason builds appear to hang.
    let load = status
        .load_averages
        .map(|l| format!("{:.2}/{:.2}/{:.2}", l[0], l[1], l[2]))
        .unwrap_or_else(|| "unavailable".to_string());
    let busy = status.active_runs.min(status.max_runs);
    checks.push(Check {
        name: "Capacity",
        severity: None,
        detail: format!(
            "{busy}/{} run slots used, {} queued, {} cores, load {load}",
            status.max_runs,
            status.queue_depth,
            status
                .cpu_count
                .map(|c| c.to_string())
                .unwrap_or_else(|| "?".into())
        ),
        hint: None,
    });

    if status.queue_depth > 0 {
        checks.push(Check::warn(
            "Queue",
            format!("{} run(s) waiting on a lock", status.queue_depth),
            "a long-running build or a stale lock is holding the workspace",
        ));
    }

    checks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(
        token: &'a str,
        toolchains: &'a [(String, String)],
        plaintext: bool,
        tls: Option<&'a config::TlsConfig>,
    ) -> DoctorInput<'a> {
        DoctorInput {
            host: "127.0.0.1:9876",
            token,
            config_path: None,
            project_name: "demo",
            project_dir: Path::new("/tmp/demo"),
            toolchains,
            tls,
            plaintext_token_in_config: plaintext,
        }
    }

    fn find<'a>(checks: &'a [Check], name: &str) -> &'a Check {
        checks
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("no {name} check in {checks:?}"))
    }

    #[test]
    fn missing_token_is_a_failure_with_an_actionable_hint() {
        let checks = local_checks(&input("", &[], false, None));
        let auth = find(&checks, "Authentication");
        assert_eq!(auth.severity, Some(Severity::Fail));
        let hint = auth.hint.as_deref().unwrap_or_default();
        assert!(
            hint.contains("FARHAND_TOKEN"),
            "hint should name the env var: {hint}"
        );
    }

    #[test]
    fn plaintext_token_warns_but_does_not_block() {
        let checks = local_checks(&input("secret", &[], true, None));
        let auth = find(&checks, "Authentication");
        assert_eq!(auth.severity, Some(Severity::Warn));
        // The value itself must never appear in the report.
        assert!(
            !auth.detail.contains("secret"),
            "the report leaked the token"
        );
    }

    #[test]
    fn token_over_cleartext_is_flagged_unless_loopback_is_obvious() {
        let clear = local_checks(&input("secret", &[], false, None));
        assert_eq!(find(&clear, "Transport").severity, Some(Severity::Warn));

        let tls = config::TlsConfig {
            enabled: true,
            ..Default::default()
        };
        let secured = local_checks(&input("secret", &[], false, Some(&tls)));
        assert_eq!(find(&secured, "Transport").severity, None);
    }

    #[test]
    fn toolchain_declarations_are_validated() {
        let good: Vec<(String, String)> = vec![("rust".into(), "1.88.0".into())];
        let checks = local_checks(&input("t", &good, false, None));
        assert_eq!(find(&checks, "Toolchains").severity, None);

        let bad: Vec<(String, String)> = vec![
            ("rust".into(), "1.88.0".into()),
            ("cobol".into(), "1.0".into()),
            ("node".into(), "  ".into()),
        ];
        let checks = local_checks(&input("t", &bad, false, None));
        let tc = find(&checks, "Toolchains");
        assert_eq!(tc.severity, Some(Severity::Warn));
        let detail = &tc.detail;
        assert!(detail.contains("cobol"), "{detail}");
        assert!(detail.contains("no version"), "{detail}");
    }

    #[test]
    fn severity_orders_fail_above_warn_above_clean() {
        // "No severity" is the absence of a finding, and the report tallies
        // problems from these, so the ordering has to hold.
        let clean: Option<Severity> = None;
        let warn = Some(Severity::Warn);
        let fail = Some(Severity::Fail);
        assert!(clean < warn);
        assert!(warn < fail);
    }
}
