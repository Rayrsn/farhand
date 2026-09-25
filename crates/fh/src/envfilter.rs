//! Environment variable forwarding policy for the client.
//!
//! Farhand forwards the local environment to the agent by default so remote
//! builds behave like local ones. That convenience is a leak risk for ambient
//! credentials, so this module applies a denylist of session/OS variables plus
//! pattern-based credential matching, and exposes a dry-run listing via
//! `fh --print-env`. Explicit `-e VAR=value` overrides always win over the
//! denylist.

/// Session/OS/terminal variables that are never forwarded implicitly.
const FILTERED_ENV_VARS: &[&str] = &[
    // Core OS / User session
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "PWD",
    "OLDPWD",
    "TMPDIR",
    "TEMP",
    "TMP",
    "TERM",
    "TERMCAP",
    "SHLVL",
    "_",
    // Farhand internals
    "FARHAND_HOST",
    "FARHAND_TOKEN",
    "FARHAND_WORKDIR",
    "FARHAND_LISTEN",
    "FARHAND_MAX_DISK_GB",
    "FARHAND_WORKSPACE_TTL_DAYS",
    // SSH & Terminal session
    "SSH_AUTH_SOCK",
    "SSH_AGENT_PID",
    "SSH_CONNECTION",
    "SSH_CLIENT",
    "SSH_TTY",
    // GUI / Desktop environments
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "XAUTHORITY",
    "XDG_RUNTIME_DIR",
    "XDG_SESSION_ID",
    "XDG_DATA_DIRS",
    "XDG_CONFIG_DIRS",
    "XDG_STATE_HOME",
    "XDG_CACHE_HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    // Editor / IDE specifics
    "VSCODE_INJECTION",
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "COLORTERM",
    "ANTIGRAVITY_SOURCE_METADATA",
    // Well-known credential variables that pattern rules would miss
    "PGPASSWORD",
    "MYSQL_PWD",
    "REDISCLI_AUTH",
    "GPG_PASSPHRASE",
];

/// Ambient variables with these prefixes are treated as infrastructure
/// credentials and never forwarded implicitly.
const CREDENTIAL_ENV_PREFIXES: &[&str] = &[
    "AWS_",
    "AZURE_",
    "GCP_",
    "GOOGLE_",
    "GITHUB_",
    "GITLAB_",
    "DOCKER_",
    "NPM_",
    "PYPI_",
    "VERCEL_",
    "NETLIFY_",
    "SUPABASE_",
    "STRIPE_",
    "SLACK_",
    "TWILIO_",
    "SENDGRID_",
    "SNYK_",
    "INFISICAL_",
    "DATABASE_",
    "HOMEBREW_",
];

/// Ambient variables with these suffixes are treated as credentials and never
/// forwarded implicitly. Deliberately excludes a bare `_KEY` to avoid false
/// positives on non-credential variables; specific forms (`_API_KEY`,
/// `_ACCESS_KEY`, `_PRIVATE_KEY`) are covered explicitly.
const CREDENTIAL_ENV_SUFFIXES: &[&str] = &[
    "_TOKEN",
    "_SECRET",
    "_PASSWORD",
    "_PASSWD",
    "_PASSPHRASE",
    "_API_KEY",
    "_ACCESS_KEY",
    "_PRIVATE_KEY",
    "_CREDENTIALS",
    "_CLIENT_SECRET",
];

/// Whether an environment variable name is excluded from implicit forwarding.
pub fn is_env_filtered(name: &str) -> bool {
    if FILTERED_ENV_VARS.contains(&name) || name.starts_with("FARHAND_") {
        return true;
    }
    if CREDENTIAL_ENV_PREFIXES.iter().any(|p| name.starts_with(p)) {
        return true;
    }
    CREDENTIAL_ENV_SUFFIXES
        .iter()
        .any(|s| name.to_uppercase().ends_with(s))
}

/// Compute the environment forwarded to the agent.
///
/// `ambient` is the caller's local environment (passed in for testability).
/// Layering: implicit ambient vars (filtered) → config `env:` block →
/// explicit `-e KEY=VAL` / `-e KEY` (always wins, even over the denylist).
/// Returns `None` when nothing would be forwarded.
pub fn collect_forward_env(
    ambient: impl IntoIterator<Item = (String, String)>,
    no_env_flag: bool,
    config_forward_env: bool,
    config_env: &std::collections::HashMap<String, String>,
    cli_env: &[String],
) -> Option<std::collections::HashMap<String, String>> {
    let mut map = std::collections::HashMap::new();

    // 1. If ambient forwarding is enabled (default), collect non-filtered local env vars
    if !no_env_flag && config_forward_env {
        for (k, v) in ambient {
            if !is_env_filtered(&k) {
                map.insert(k, v);
            }
        }
    }

    // 2. Overlay environment variables defined in .farhand.yaml
    for (k, v) in config_env {
        map.insert(k.clone(), v.clone());
    }

    // 3. Overlay explicit CLI flags: -e KEY=VAL or -e KEY (takes value from current local env)
    for entry in cli_env {
        if let Some((k, v)) = entry.split_once('=') {
            map.insert(k.to_string(), v.to_string());
        } else if let Ok(v) = std::env::var(entry) {
            map.insert(entry.clone(), v);
        }
    }

    if map.is_empty() {
        None
    } else {
        Some(map)
    }
}

/// Names (sorted) of the variables that would be forwarded under the current
/// policy — used by `fh --print-env` and verbose telemetry. Values are never
/// included.
pub fn forwarded_env_names(
    ambient: impl IntoIterator<Item = (String, String)>,
    no_env_flag: bool,
    config_forward_env: bool,
    config_env: &std::collections::HashMap<String, String>,
    cli_env: &[String],
) -> Vec<String> {
    match collect_forward_env(
        ambient,
        no_env_flag,
        config_forward_env,
        config_env,
        cli_env,
    ) {
        Some(map) => {
            let mut names: Vec<String> = map.keys().cloned().collect();
            names.sort();
            names
        }
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ambient(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn empty_map() -> std::collections::HashMap<String, String> {
        std::collections::HashMap::new()
    }

    #[test]
    fn filters_os_session_and_farhand_vars() {
        for name in ["PATH", "HOME", "SSH_AUTH_SOCK", "FARHAND_TOKEN", "DISPLAY"] {
            assert!(is_env_filtered(name), "{name} should be filtered");
        }
        assert!(is_env_filtered("FARHAND_ANYTHING"));
    }

    #[test]
    fn filters_credential_prefixes() {
        for name in [
            "AWS_SECRET_ACCESS_KEY",
            "AZURE_CLIENT_SECRET",
            "GITHUB_TOKEN",
            "NPM_CONFIG_//registry.npmjs.org/:_authToken",
            "PYPI_PASSWORD",
            "DATABASE_URL",
            "INFISICAL_TOKEN",
        ] {
            assert!(is_env_filtered(name), "{name} should be filtered");
        }
    }

    #[test]
    fn filters_credential_suffixes() {
        for name in [
            "MY_API_TOKEN",
            "STRIPE_SECRET",
            "PGPASSWORD",
            "CERT_PASSWORD",
            "OPENAI_API_KEY",
            "AWS_ACCESS_KEY",
            "MY_CREDS_CREDENTIALS",
        ] {
            assert!(is_env_filtered(name), "{name} should be filtered");
        }
    }

    #[test]
    fn allows_ordinary_build_variables() {
        for name in [
            "CARGO_BUILD_JOBS",
            "RUSTFLAGS",
            "NODE_OPTIONS",
            "CI",
            "MAKEFLAGS",
            "SSH_KEYSIZE", // _KEY suffix intentionally not over-blocked
            "MY_KEYSPACE",
        ] {
            assert!(!is_env_filtered(name), "{name} should be forwarded");
        }
    }

    #[test]
    fn ambient_forwarding_respects_flags() {
        let env = ambient(&[("RUSTFLAGS", "-O"), ("MY_API_TOKEN", "zzz")]);
        let forwarded = collect_forward_env(env, false, true, &empty_map(), &[]).unwrap();
        assert!(forwarded.contains_key("RUSTFLAGS"));
        assert!(!forwarded.contains_key("MY_API_TOKEN"));

        // --no-env disables the ambient layer entirely.
        let env = ambient(&[("RUSTFLAGS", "-O")]);
        assert!(collect_forward_env(env, true, true, &empty_map(), &[]).is_none());
    }

    #[test]
    fn explicit_overrides_win_over_denylist() {
        let env = ambient(&[("MY_API_TOKEN", "ambient-secret")]);
        let forwarded = collect_forward_env(
            env,
            false,
            true,
            &empty_map(),
            &["MY_API_TOKEN=explicit".to_string()],
        )
        .unwrap();
        assert_eq!(
            forwarded.get("MY_API_TOKEN").map(String::as_str),
            Some("explicit")
        );
    }

    #[test]
    fn config_env_and_empty_result() {
        let mut cfg_env = empty_map();
        cfg_env.insert("BASE_URL".to_string(), "http://x".to_string());
        let forwarded = collect_forward_env(ambient(&[]), false, false, &cfg_env, &[]).unwrap();
        assert_eq!(forwarded.len(), 1);

        // Nothing to forward at all → None (agent receives no env frame).
        assert!(collect_forward_env(ambient(&[]), false, false, &empty_map(), &[]).is_none());
    }

    #[test]
    fn forwarded_names_are_sorted() {
        let env = ambient(&[("ZZZ", "1"), ("AAA", "2"), ("MY_SECRET", "3")]);
        let names = forwarded_env_names(env, false, true, &empty_map(), &[]);
        assert_eq!(names, vec!["AAA", "ZZZ"]);
    }
}
