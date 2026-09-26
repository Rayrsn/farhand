//! Progress reporting for the slow part of a run.
//!
//! Packing the delta is the only phase with real, incremental work to report,
//! so that is what this drives. Rendering is suppressed unless stdout is a
//! terminal: a progress bar written into a redirected log or a CI log is
//! unreadable garbage, and "only when someone is watching" is the rule that
//! keeps both honest.

use std::cell::Cell;
use std::io::IsTerminal;
use std::time::Instant;

/// A single-line progress bar, or nothing at all when there is no terminal.
pub struct Progress {
    enabled: bool,
    label: &'static str,
    started: Instant,
    /// Width of the last line drawn, so the next one can pad over it.
    last_width: Cell<usize>,
}

impl Progress {
    /// Start reporting progress for `label`, or stay silent if stdout is not
    /// a terminal.
    pub fn start(label: &'static str) -> Self {
        Progress {
            enabled: std::io::stdout().is_terminal(),
            label,
            started: Instant::now(),
            last_width: Cell::new(0),
        }
    }

    /// Whether anything will actually be drawn. Callers use this to avoid
    /// doing work purely to animate it.
    pub fn is_visible(&self) -> bool {
        self.enabled
    }

    /// Draw `done`/`total`.
    pub fn update(&self, done: usize, total: usize) {
        if !self.enabled {
            return;
        }
        const WIDTH: usize = 24;
        let ratio = if total == 0 {
            1.0
        } else {
            (done as f64 / total as f64).clamp(0.0, 1.0)
        };
        let filled = (ratio * WIDTH as f64).round() as usize;
        let line = format!(
            "[{}{}] {:>3}% {done}/{total} {}  {:.1}s",
            "=".repeat(filled),
            " ".repeat(WIDTH.saturating_sub(filled)),
            (ratio * 100.0).round() as u64,
            self.label,
            self.started.elapsed().as_secs_f32(),
        );
        self.draw(&line);
    }

    /// Clear the progress line.
    pub fn clear(&self) {
        if !self.enabled {
            return;
        }
        use std::io::Write;
        let mut out = std::io::stdout();
        let _ = write!(out, "\r{}\r", " ".repeat(self.last_width.get()));
        let _ = out.flush();
        self.last_width.set(0);
    }

    /// Replace the progress line with a final message.
    pub fn finish(&self, message: &str) {
        self.clear();
        println!("{message}");
    }

    fn draw(&self, line: &str) {
        use std::io::Write;
        let mut out = std::io::stdout();
        // Pad over whatever the previous line occupied, so a line that shrinks
        // (a smaller count, a shorter label) does not leave debris behind.
        let padding = " ".repeat(self.last_width.get().saturating_sub(line.len()));
        let _ = write!(out, "\r{line}{padding}");
        let _ = out.flush();
        self.last_width
            .set(line.chars().count() + padding.chars().count());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_drawn_without_a_terminal() {
        // The test harness captures stdout, so `is_visible` must be false and
        // every method must be a safe no-op rather than emitting bytes.
        let p = Progress::start("packing");
        assert!(!p.is_visible());
        p.update(1, 10);
        p.update(10, 10);
        p.clear();
        p.finish("done");
    }
}
