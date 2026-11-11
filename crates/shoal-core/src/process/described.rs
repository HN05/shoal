//! What a person recognizes processes by: the command line and how long
//! each has run.
use std::collections::HashMap;

use anyhow::Result;

use super::identity::{self, details};

/// A process as a person recognizes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Description {
    pub command: String,
    /// Seconds since it started.
    pub elapsed: u64,
}

/// The processes of `pids` that still run, by PID.
pub fn describe(pids: &[u32]) -> Result<HashMap<u32, Description>> {
    let mut described = HashMap::new();
    for &pid in pids {
        if let Some(description) = description(pid)? {
            described.insert(pid, description);
        }
    }
    Ok(described)
}

/// `pid` as a person recognizes it, `None` once it has exited. One whose
/// arguments the kernel hides shows the file it runs.
fn description(pid: u32) -> Result<Option<Description>> {
    let Some(elapsed) = details::elapsed(pid)? else {
        return Ok(None);
    };
    let command = match details::arguments(pid) {
        Some(arguments) => command_line(&arguments),
        None => match identity::executable(pid) {
            Some(executable) => executable.to_string_lossy().into_owned(),
            None => return Ok(None),
        },
    };
    Ok(Some(Description {
        command: printable(&command),
        elapsed: elapsed.as_secs(),
    }))
}

/// `arguments` separated by spaces, as `ps` shows a command.
fn command_line(arguments: &[std::ffi::OsString]) -> String {
    arguments
        .iter()
        .map(|argument| argument.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}

/// `text` with each control character shown as `?`, as `ps` shows it, so a
/// newline or escape sequence in an argument cannot change the terminal or
/// split a listing.
fn printable(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

#[cfg(test)]
mod tests;
