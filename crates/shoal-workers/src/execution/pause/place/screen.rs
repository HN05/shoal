//! The status block of an asleep agent's place, drawn under what the agent
//! last showed: what wakes the agent, why it sleeps, what else its restore
//! waits for, and the keys that wake or stop it.
use std::io::Write;

use crate::protocol::Protection;

/// Hide the cursor and reset text attributes, which the agent's interface
/// may have left behind.
const OPEN: &str = "\x1b[?25l\x1b[0m";
/// Show the cursor again.
const CLOSE: &str = "\x1b[?25h";
/// Clear from the cursor to the end of the screen.
const CLEAR_BELOW: &str = "\x1b[J";
/// Switch the terminal's line wrapping off while the block draws, so each
/// row the block wraps its lines into takes exactly one terminal row, and on
/// again after it.
const NO_WRAP: &str = "\x1b[?7l";
const WRAP: &str = "\x1b[?7h";
/// The widest rule that separates the block from the agent's output.
const RULE_WIDTH: usize = 48;
/// The width the block wraps to on a terminal that does not say its own,
/// and the least it wraps to, beyond which the terminal cuts rows off.
const DEFAULT_WIDTH: usize = 80;
const MIN_WIDTH: usize = 20;

pub(super) struct Screen {
    reason: String,
    resumes_when: String,
    notes: Vec<String>,
    wake: Option<Wake>,
    /// The rows the block took when it last drew, which its next draw
    /// moves up over.
    drawn: usize,
}

/// How waking the agent from its place went.
pub(super) enum Wake {
    /// The daemon starts its restore.
    Waking,
    /// The wake could not reach the daemon, for this reason.
    Unsent(String),
}

impl Screen {
    pub(super) fn new(protection: &Protection) -> Self {
        Self {
            reason: protection.reason.clone(),
            resumes_when: protection.resumes_when.clone(),
            notes: Vec::new(),
            wake: None,
            drawn: 0,
        }
    }

    /// Take the terminal back from what the agent's interface left on it,
    /// keeping its output, then draw the block on a line of its own under
    /// that output.
    pub(super) fn open(&mut self) {
        let reset = format!("{OPEN}{}\r\n", shoal_core::execution::INPUT_MODES_RESET);
        self.draw(&reset);
    }

    pub(super) fn note(&mut self, line: String) {
        self.notes.push(line);
        self.draw("");
    }

    pub(super) fn wake(&mut self, wake: Wake) {
        self.wake = Some(wake);
        self.draw("");
    }

    /// Redraw the block in place after `prefix`. The terminal may have
    /// closed with the session.
    fn draw(&mut self, prefix: &str) {
        let width = shoal_core::execution::terminal_width(std::io::stderr())
            .unwrap_or(DEFAULT_WIDTH)
            .max(MIN_WIDTH);
        let frame = self.frame(width);
        let mut stderr = std::io::stderr().lock();
        let _ = write!(stderr, "{prefix}{frame}");
        let _ = stderr.flush();
    }

    /// What redrawing the block `width` columns wide writes: up over the
    /// rows it took before, then the block in their place.
    fn frame(&mut self, width: usize) -> String {
        let rows = self.rows(width);
        let up = match self.drawn {
            0 => String::new(),
            drawn => format!("\r\x1b[{drawn}A"),
        };
        self.drawn = rows.len();
        // The terminal's output settings may not turn a newline into a
        // carriage return too.
        let block: String = rows.iter().map(|row| format!("{row}\r\n")).collect();
        format!("{up}{CLEAR_BELOW}{NO_WRAP}{block}{WRAP}")
    }

    /// What the block shows `width` columns wide, one terminal row each:
    /// the rule, then its lines wrapped to the width.
    fn rows(&self, width: usize) -> Vec<String> {
        let rule = "─".repeat(RULE_WIDTH.min(width));
        let wrapped = self.lines().into_iter().flat_map(|line| wrap(&line, width));
        std::iter::once(rule).chain(wrapped).collect()
    }

    /// The block's lines under its rule.
    fn lines(&self) -> Vec<String> {
        let mut lines = vec![
            format!(" Agent asleep    Wakes when {}", self.resumes_when),
            format!(" Why             {}", self.reason),
        ];
        lines.extend(self.notes.iter().map(|note| format!(" {note}")));
        match &self.wake {
            Some(Wake::Waking) => lines.push(" Waking agent".to_owned()),
            Some(Wake::Unsent(error)) => {
                lines.push(format!(" Cannot ask the daemon to wake the agent: {error}"));
            }
            None => {}
        }
        lines.push(" Enter  Wake now    Ctrl-C  Stop agent".to_owned());
        lines
    }
}

impl Drop for Screen {
    /// Give the terminal back to the restored agent or the shell with its
    /// cursor showing, leaving the block in its history.
    fn drop(&mut self) {
        let mut stderr = std::io::stderr().lock();
        let _ = write!(stderr, "{CLOSE}");
        let _ = stderr.flush();
    }
}

/// `line` in rows of at most `width` characters, broken at the last space
/// that fits, or within a word that does not, each row after the first
/// indented as the block's lines are.
fn wrap(line: &str, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    let mut rest: Vec<char> = line.chars().collect();
    while rest.len() > width {
        let row = match rest[1..width].iter().rposition(|c| *c == ' ') {
            Some(space) => {
                let space = space + 1;
                let row: String = rest[..space].iter().collect();
                rest.drain(..=space);
                row
            }
            None => rest.drain(..width).collect(),
        };
        rows.push(row);
        rest.insert(0, ' ');
    }
    rows.push(rest.into_iter().collect());
    rows
}

#[cfg(test)]
mod tests;
