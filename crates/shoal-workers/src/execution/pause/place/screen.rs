//! The status screen of an asleep agent's place: why the agent sleeps, when
//! it resumes, what else its restore waits for, and the keys that wake or
//! stop it.
use std::io::Write;

use crate::protocol::Protection;

/// Leave the alternate screen, show the cursor and reset text attributes,
/// which the agent's interface may have left behind.
const LEAVE_INTERFACE: &str = "\x1b[?1049l\x1b[?25h\x1b[0m";
/// Move home and clear the screen, keeping the scrollback.
const CLEAR: &str = "\x1b[H\x1b[2J";

pub(super) struct Screen {
    reason: String,
    resumes_when: String,
    notes: Vec<String>,
    wake: Option<Wake>,
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
        }
    }

    /// Take the terminal back from what the agent's interface left on it,
    /// then draw the screen.
    pub(super) fn open(&self) {
        let reset = format!(
            "{LEAVE_INTERFACE}{}",
            shoal_core::execution::INPUT_MODES_RESET
        );
        self.write(&reset);
    }

    pub(super) fn note(&mut self, line: String) {
        self.notes.push(line);
        self.write("");
    }

    pub(super) fn wake(&mut self, wake: Wake) {
        self.wake = Some(wake);
        self.write("");
    }

    /// Redraw the screen after `prefix`. The terminal may have closed with
    /// the session.
    fn write(&self, prefix: &str) {
        let mut stderr = std::io::stderr().lock();
        let _ = write!(stderr, "{prefix}{CLEAR}{}", self.text());
        let _ = stderr.flush();
    }

    /// What the screen shows, one line per terminal row.
    fn text(&self) -> String {
        let mut lines = vec![
            "Agent asleep".to_owned(),
            String::new(),
            format!("Reason:        {}", self.reason),
            format!("Resumes when:  {}", self.resumes_when),
        ];
        if !self.notes.is_empty() {
            lines.push(String::new());
            lines.extend(self.notes.iter().cloned());
        }
        match &self.wake {
            Some(Wake::Waking) => lines.extend([String::new(), "Waking agent".to_owned()]),
            Some(Wake::Unsent(error)) => lines.extend([
                String::new(),
                format!("Cannot ask the daemon to wake the agent: {error}"),
            ]),
            None => {}
        }
        lines.extend([
            String::new(),
            "Enter   Wake now".to_owned(),
            "Ctrl-C  Stop agent".to_owned(),
        ]);
        // The terminal's output settings may not turn a newline into a
        // carriage return too.
        lines.iter().map(|line| format!("{line}\r\n")).collect()
    }
}

#[cfg(test)]
mod tests;
