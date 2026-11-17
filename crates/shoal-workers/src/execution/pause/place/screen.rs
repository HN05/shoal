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
/// Switch line wrapping off while the block draws, so each line takes one
/// row however wide the terminal is, and on again after it.
const NO_WRAP: &str = "\x1b[?7l";
const WRAP: &str = "\x1b[?7h";
/// The rule that separates the block from the agent's output.
const RULE_WIDTH: usize = 48;

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
        let frame = self.frame();
        let mut stderr = std::io::stderr().lock();
        let _ = write!(stderr, "{prefix}{frame}");
        let _ = stderr.flush();
    }

    /// What redrawing the block writes: up over the rows it took before,
    /// then the block in their place.
    fn frame(&mut self) -> String {
        let rows = self.rows();
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

    /// What the block shows, one line per terminal row.
    fn rows(&self) -> Vec<String> {
        let mut rows = vec![
            "─".repeat(RULE_WIDTH),
            format!(" Agent asleep    Wakes when {}", self.resumes_when),
            format!(" Why             {}", self.reason),
        ];
        rows.extend(self.notes.iter().map(|note| format!(" {note}")));
        match &self.wake {
            Some(Wake::Waking) => rows.push(" Waking agent".to_owned()),
            Some(Wake::Unsent(error)) => {
                rows.push(format!(" Cannot ask the daemon to wake the agent: {error}"));
            }
            None => {}
        }
        rows.push(" Enter  Wake now    Ctrl-C  Stop agent".to_owned());
        rows
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

#[cfg(test)]
mod tests;
