//! Command construction and process lifecycle for remote commands.
//!
//! Everything here turns a `RunPayload` argument vector into an actual
//! OS process: shell selection, argument escaping, toolchain environment
//! wiring, and process-group termination on cancellation (AGENTS.md §3.4).

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;

#[cfg(windows)]
pub(crate) fn shell_escape(arg: &str) -> String {
    if arg.is_empty() {
        return "\"\"".to_string();
    }
    if arg.chars().all(|c| {
        c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | '\\' | ':' | '=' | '@')
    }) {
        return arg.to_string();
    }
    format!("\"{}\"", arg.replace('"', "\\\""))
}

#[cfg(not(windows))]
pub(crate) fn shell_escape(arg: &str) -> String {
    if arg.is_empty() {
        return "''".to_string();
    }
    if arg
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '=' | '@'))
    {
        return arg.to_string();
    }
    format!("'{}'", arg.replace('\'', "'\\''"))
}

pub fn wrap_command_with_toolchain(
    cmd_str: &str,
    toolchain: Option<&HashMap<String, String>>,
) -> String {
    let Some(toolchain) = toolchain else {
        return cmd_str.to_string();
    };
    if toolchain.is_empty() {
        return cmd_str.to_string();
    }

    #[cfg(not(unix))]
    {
        let _ = toolchain;
        cmd_str.to_string()
    }

    #[cfg(unix)]
    {
        let mut prefixes: Vec<String> = Vec::new();
        for (lang, ver) in toolchain {
            let l = lang.to_ascii_lowercase();
            match l.as_str() {
                "node" | "nodejs" => {
                    prefixes.push(format!(
                        "(export NVM_DIR=\"$HOME/.nvm\"; [ -s \"$NVM_DIR/nvm.sh\" ] && \\. \"$NVM_DIR/nvm.sh\" && nvm use {} >/dev/null 2>&1) || (which fnm >/dev/null 2>&1 && eval \"$(fnm env)\" && fnm use {} >/dev/null 2>&1) || true",
                        ver, ver
                    ));
                }
                "go" | "golang" => {
                    prefixes.push(format!(
                        "(which goenv >/dev/null 2>&1 && export GOENV_VERSION={} && eval \"$(goenv init -)\") || true",
                        ver
                    ));
                }
                "python" | "pyenv" => {
                    prefixes.push(
                        "(which pyenv >/dev/null 2>&1 && eval \"$(pyenv init -)\") || true"
                            .to_string(),
                    );
                }
                _ => {}
            }
        }

        if prefixes.is_empty() {
            cmd_str.to_string()
        } else {
            format!("{} && {}", prefixes.join(" && "), cmd_str)
        }
    }
}

pub fn apply_toolchain_env(cmd: &mut Command, toolchain: Option<&HashMap<String, String>>) {
    if let Some(tc) = toolchain {
        for (lang, ver) in tc {
            let l = lang.to_ascii_lowercase();
            match l.as_str() {
                "rust" | "rustup" => {
                    cmd.env("RUSTUP_TOOLCHAIN", ver);
                }
                "python" | "pyenv" => {
                    cmd.env("PYENV_VERSION", ver);
                }
                "node" | "nodejs" => {
                    cmd.env("NODE_VERSION", ver);
                }
                _ => {}
            }
            let env_key = format!("FARHAND_TOOLCHAIN_{}", l.to_ascii_uppercase());
            cmd.env(env_key, ver);
        }
    }
}

pub fn apply_toolchain_pty(
    cmd_builder: &mut portable_pty::CommandBuilder,
    toolchain: Option<&HashMap<String, String>>,
) {
    if let Some(tc) = toolchain {
        for (lang, ver) in tc {
            let l = lang.to_ascii_lowercase();
            match l.as_str() {
                "rust" | "rustup" => {
                    cmd_builder.env("RUSTUP_TOOLCHAIN", ver);
                }
                "python" | "pyenv" => {
                    cmd_builder.env("PYENV_VERSION", ver);
                }
                "node" | "nodejs" => {
                    cmd_builder.env("NODE_VERSION", ver);
                }
                _ => {}
            }
            let env_key = format!("FARHAND_TOOLCHAIN_{}", l.to_ascii_uppercase());
            cmd_builder.env(env_key, ver);
        }
    }
}

/// Parse a custom shell invocation (e.g. `/bin/sh -c`) into argv tokens.
/// Returns `None` for empty or whitespace-only input — a misconfiguration the
/// daemon rejects at startup; library callers fall back to the default shell.
pub fn parse_custom_shell(shell: &str) -> Option<Vec<String>> {
    let tokens: Vec<String> = shell.split_whitespace().map(str::to_string).collect();
    if tokens.is_empty() {
        None
    } else {
        Some(tokens)
    }
}

pub fn build_shell_command(
    cwd: &Path,
    argv: &[String],
    custom_shell: Option<&str>,
    toolchain: Option<&HashMap<String, String>>,
) -> Command {
    let joined_cmd = argv
        .iter()
        .map(|a| shell_escape(a))
        .collect::<Vec<_>>()
        .join(" ");
    let wrapped_cmd = wrap_command_with_toolchain(&joined_cmd, toolchain);

    let mut cmd = if let Some(shell_override) = custom_shell {
        let shell_tokens = parse_custom_shell(shell_override)
            .unwrap_or_else(|| vec!["/bin/sh".to_string(), "-c".to_string()]);
        let mut c = Command::new(&shell_tokens[0]);
        for part in &shell_tokens[1..] {
            c.arg(part);
        }
        c.arg(&wrapped_cmd);
        c
    } else if cfg!(windows) {
        let mut c = Command::new("cmd.exe");
        #[cfg(windows)]
        c.raw_arg(format!("/C \"{}\"", wrapped_cmd));
        #[cfg(not(windows))]
        c.arg("/C").arg(&wrapped_cmd);
        c
    } else {
        let mut c = Command::new("/bin/sh");
        c.arg("-c").arg(&wrapped_cmd);
        c
    };

    cmd.current_dir(cwd);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    #[cfg(unix)]
    cmd.process_group(0);

    cmd
}

pub fn build_raw_shell_command(
    cwd: &Path,
    raw_cmd: &str,
    custom_shell: Option<&str>,
    toolchain: Option<&HashMap<String, String>>,
) -> Command {
    let wrapped_cmd = wrap_command_with_toolchain(raw_cmd, toolchain);
    let mut cmd = if let Some(shell_override) = custom_shell {
        let shell_tokens = parse_custom_shell(shell_override)
            .unwrap_or_else(|| vec!["/bin/sh".to_string(), "-c".to_string()]);
        let mut c = Command::new(&shell_tokens[0]);
        for part in &shell_tokens[1..] {
            c.arg(part);
        }
        c.arg(&wrapped_cmd);
        c
    } else if cfg!(windows) {
        let mut c = Command::new("cmd.exe");
        #[cfg(windows)]
        c.raw_arg(format!("/C \"{}\"", wrapped_cmd));
        #[cfg(not(windows))]
        c.arg("/C").arg(&wrapped_cmd);
        c
    } else {
        let mut c = Command::new("/bin/sh");
        c.arg("-c").arg(&wrapped_cmd);
        c
    };

    cmd.current_dir(cwd);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    #[cfg(unix)]
    cmd.process_group(0);

    cmd
}

/// Terminate a remote command's whole process group (AGENTS.md §3.4).
///
/// SIGTERM first, then SIGKILL after 3s: the negative pid signals the
/// process *group*, which is why `build_command` sets `process_group(0)` —
/// that makes the child a group leader whose pgid equals its pid, so the
/// compiler tree (rustc → linker → build script) dies together instead of
/// leaving orphans behind.
#[allow(unsafe_code)] // FFI: kill(2) on our own child's process group — SAFETY comments inside.
pub async fn kill_process_group(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // SAFETY: `pid` is the id of a child this daemon spawned with
        // `process_group(0)`, so `-pid` addresses that child's process group
        // and can only signal processes we own. `kill` is async-signal-safe
        // and has no memory preconditions.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGTERM);
        }
        tokio::select! {
            _ = child.wait() => {}
            _ = tokio::time::sleep(std::time::Duration::from_secs(3)) => {
                // SAFETY: as above; the process group may still contain
                // descendants that ignored SIGTERM.
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
            }
        }
    }
    #[cfg(windows)]
    if let Some(pid) = child.id() {
        let _ = tokio::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .output()
            .await;
        let _ = child.kill().await;
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = child.kill().await;
    }
}

pub fn resolve_shell_executable(requested: &str) -> String {
    if requested != "$SHELL" && !requested.is_empty() {
        if Path::new(requested).is_file() {
            return requested.to_string();
        }
        if !requested.contains('/') && !requested.contains('\\') {
            for dir in &["/bin", "/usr/bin", "/usr/local/bin", "/opt/homebrew/bin"] {
                let candidate = Path::new(dir).join(requested);
                if candidate.is_file() {
                    return candidate.to_string_lossy().to_string();
                }
            }
        }
    }

    if let Ok(sh) = std::env::var("SHELL") {
        if Path::new(&sh).is_file() {
            return sh;
        }
    }
    for candidate in &[
        "/bin/zsh",
        "/bin/bash",
        "/usr/bin/zsh",
        "/usr/bin/bash",
        "/bin/sh",
    ] {
        if Path::new(candidate).is_file() {
            return candidate.to_string();
        }
    }
    #[cfg(windows)]
    {
        "powershell.exe".to_string()
    }
    #[cfg(not(windows))]
    {
        "/bin/sh".to_string()
    }
}
