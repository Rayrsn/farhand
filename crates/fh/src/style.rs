//! Terminal styling, disabled when it would be unwanted.
//!
//! Colour is decided once, here, rather than per call site: the alternative is
//! escape codes sprinkled through the output, which is how CI logs and
//! redirected output end up full of them.
//!
//! Honours the de-facto standards, in order:
//!
//! * `NO_COLOR` (any value) — <https://no-color.org>
//! * `CLICOLOR=0`
//! * `TERM=dumb`
//! * not a terminal (piped or redirected)
//! * `CLICOLOR_FORCE` overrides the above, for the rare case someone wants
//!   colour in a pipe on purpose
//!
//! Every helper is a no-op when styling is off, so callers can wrap output
//! unconditionally.

use std::sync::LazyLock;

/// The decision itself, as a pure function of the environment and whether we
/// are attached to a terminal.
///
/// Split out from [`ENABLED`] so it can be tested without depending on where
/// the test happens to run. The earlier version asserted `!enabled()`, which
/// passed under CI (no tty) and failed for anyone running `cargo test` in a
/// terminal.
fn should_style(
    no_color: Option<&std::ffi::OsStr>,
    clicolor: Option<&std::ffi::OsStr>,
    term: Option<&std::ffi::OsStr>,
    force: Option<&std::ffi::OsStr>,
    is_tty: bool,
) -> bool {
    if force.is_some_and(|v| v != "0") {
        return true;
    }
    if no_color.is_some() {
        return false;
    }
    if clicolor.is_some_and(|v| v == "0") {
        return false;
    }
    if term.is_some_and(|v| v == "dumb") {
        return false;
    }
    is_tty
}

/// Whether ANSI escapes may be emitted.
static ENABLED: LazyLock<bool> = LazyLock::new(|| {
    should_style(
        std::env::var_os("NO_COLOR").as_deref(),
        std::env::var_os("CLICOLOR").as_deref(),
        std::env::var_os("TERM").as_deref(),
        std::env::var_os("CLICOLOR_FORCE").as_deref(),
        std::io::IsTerminal::is_terminal(&std::io::stdout()),
    )
});

fn enabled() -> bool {
    *ENABLED
}

/// Wrap `text` in an SGR sequence, or return it unchanged.
fn paint(codes: &str, text: &str) -> String {
    if enabled() {
        format!("\u{1b}[{codes}m{text}\u{1b}[0m")
    } else {
        text.to_string()
    }
}

pub fn red(text: &str) -> String {
    paint("31", text)
}

pub fn green(text: &str) -> String {
    paint("32", text)
}

pub fn yellow(text: &str) -> String {
    paint("33", text)
}

pub fn blue(text: &str) -> String {
    paint("34", text)
}

pub fn bold(text: &str) -> String {
    paint("1", text)
}

pub fn dim(text: &str) -> String {
    paint("2", text)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The decision, tested directly rather than through the ambient
    /// environment. The previous version asserted `!enabled()`, which depends
    /// on whether the test runner has a terminal: green in CI, red for anyone
    /// running `cargo test` in their shell.
    #[test]
    fn the_decision_honours_each_variable() {
        fn s(v: &str) -> Option<&std::ffi::OsStr> {
            Some(std::ffi::OsStr::new(v))
        }

        assert!(
            should_style(None, None, None, None, true),
            "a tty gets colour"
        );
        assert!(
            !should_style(None, None, None, None, false),
            "a pipe does not"
        );

        // Every opt-out beats an implicit tty.
        assert!(!should_style(s("1"), None, None, None, true), "NO_COLOR");
        assert!(!should_style(None, s("0"), None, None, true), "CLICOLOR=0");
        assert!(
            !should_style(None, None, s("dumb"), None, true),
            "TERM=dumb"
        );
        // NO_COLOR is honoured for *any* value, including empty.
        assert!(
            !should_style(s(""), None, None, None, true),
            "NO_COLOR empty"
        );

        // The explicit override wins over all of them.
        assert!(
            should_style(s("1"), s("0"), s("dumb"), s("1"), false),
            "CLICOLOR_FORCE"
        );
    }

    /// The guarantee callers rely on: with styling off, output must be exactly
    /// the input. Anything else corrupts CI logs and `--json` consumers.
    #[test]
    fn helpers_are_identity_when_disabled() {
        for text in ["plain", "with \"quotes\"", "emoji 🎉", ""] {
            for f in [red, green, yellow, blue, bold, dim] {
                assert_eq!(f(text), text, "{f:?} altered its input");
            }
        }
    }
}
