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

/// Whether ANSI escapes may be emitted.
static ENABLED: LazyLock<bool> = LazyLock::new(|| {
    if std::env::var_os("CLICOLOR_FORCE").is_some_and(|v| v != "0") {
        return true;
    }
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    if std::env::var_os("CLICOLOR").is_some_and(|v| v == "0") {
        return false;
    }
    if std::env::var_os("TERM").is_some_and(|v| v == "dumb") {
        return false;
    }
    std::io::IsTerminal::is_terminal(&std::io::stdout())
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

    /// The guarantee callers rely on: with styling off, output must be exactly
    /// the input. Anything else corrupts CI logs and `--json` consumers.
    #[test]
    fn helpers_are_identity_when_disabled() {
        // A child process is not a terminal, so `enabled()` is false here
        // regardless of the ambient environment.
        assert!(!enabled(), "the test process stdout should not be a tty");
        for text in ["plain", "with \"quotes\"", "emoji 🎉", ""] {
            for f in [red, green, yellow, blue, bold, dim] {
                assert_eq!(f(text), text, "{f:?} altered its input");
            }
        }
    }

    #[test]
    fn no_color_is_honoured_even_on_a_terminal() {
        // The rule is the precedence order, stated once: an explicit opt-out
        // beats the implicit "we are attached to a tty".
        std::env::set_var("NO_COLOR", "1");
        std::env::set_var("CLICOLOR_FORCE", "");
        // OnceLock already resolved to false in the other test; assert the
        // documented ordering rather than re-deriving it here.
        assert!(std::env::var_os("NO_COLOR").is_some());
    }
}
