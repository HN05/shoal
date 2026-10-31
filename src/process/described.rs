//! What `ps` shows about processes for a person to recognize them: the
//! command line and how long each has run.
use std::collections::HashMap;

use anyhow::{Context, Result};
use tokio::process::Command;

use crate::{protocol::timing, tools::Tool};

/// A process as `ps` shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Description {
    pub command: String,
    /// Seconds since it started.
    pub elapsed: u64,
}

/// The processes of `pids` that still run, by PID.
pub async fn describe(pids: &[u32]) -> Result<HashMap<u32, Description>> {
    if pids.is_empty() {
        return Ok(HashMap::new());
    }
    let list = pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let mut command = Command::new(Tool::Ps.program());
    command
        .args(["-o", "pid=,etime=,command=", "-p", &list])
        .env("LC_ALL", "C");
    // `ps` exits 1 when one of the processes has exited; its output still
    // lists the others.
    let output = tokio::time::timeout(timing::PROCESS_INVENTORY_TIMEOUT, command.output())
        .await
        .context("describing processes timed out")?
        .context("describe processes")?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(parse_line)
        .collect())
}

/// A `pid etime command` line, whose command keeps its own spaces.
fn parse_line(line: &str) -> Option<(u32, Description)> {
    let (pid, rest) = line.trim_start().split_once(char::is_whitespace)?;
    let (time, command) = rest.trim_start().split_once(char::is_whitespace)?;
    let description = Description {
        command: command.trim().to_owned(),
        elapsed: elapsed(time)?,
    };
    Some((pid.parse().ok()?, description))
}

/// Seconds from `ps`'s `[[dd-]hh:]mm:ss` elapsed time.
fn elapsed(field: &str) -> Option<u64> {
    let (days, clock) = match field.split_once('-') {
        Some((days, clock)) => (days.parse::<u64>().ok()?, clock),
        None => (0, field),
    };
    let seconds = clock.split(':').try_fold(0_u64, |total, part| {
        Some(total * 60 + part.parse::<u64>().ok()?)
    })?;
    Some(days * 86_400 + seconds)
}

#[cfg(test)]
mod tests;
