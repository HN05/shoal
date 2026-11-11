//! What the kernel shows about a process beyond its identity: the command
//! line a person recognizes it by and that finds a terminal host's server,
//! the environment it started with, how long it has run, and its terminal
//! with that terminal's foreground process group. A process that has
//! exited, or whose records the kernel refuses, shows none of it.
use super::platform;
use anyhow::Result;
use std::{ffi::OsString, os::unix::ffi::OsStringExt, path::PathBuf, time::Duration};

/// A live process of this user with the arguments it runs with.
pub(crate) struct CommandLine {
    pub(crate) pid: u32,
    pub(crate) parent: u32,
    pub(crate) arguments: Vec<OsString>,
}

/// This user's live processes other than init and this one whose arguments
/// the kernel shows.
pub(crate) fn own_command_lines() -> Result<Vec<CommandLine>> {
    Ok(super::list_same_user()?
        .into_iter()
        .filter_map(|(pid, _, parent)| {
            Some(CommandLine {
                pid,
                parent,
                arguments: arguments(pid)?,
            })
        })
        .collect())
}

/// The arguments `pid` runs with.
pub(crate) fn arguments(pid: u32) -> Option<Vec<OsString>> {
    let arguments = platform::arguments(valid(pid)?)?;
    Some(arguments.into_iter().map(OsString::from_vec).collect())
}

/// The value of environment variable `name` that `pid` started with.
pub(crate) fn variable(pid: u32, name: &str) -> Option<OsString> {
    let prefix = format!("{name}=");
    platform::environment(valid(pid)?)
        .ok()?
        .into_iter()
        .find_map(|entry| Some(entry.strip_prefix(prefix.as_bytes())?.to_vec()))
        .map(OsString::from_vec)
}

/// How long `pid` has run.
pub(crate) fn elapsed(pid: u32) -> Result<Option<Duration>> {
    valid(pid).map_or(Ok(None), platform::elapsed)
}

/// The device of `pid`'s controlling terminal.
pub(crate) fn terminal(pid: u32) -> Result<Option<PathBuf>> {
    valid(pid).map_or(Ok(None), platform::terminal)
}

/// The foreground process group of `pid`'s controlling terminal.
pub(crate) fn foreground_group(pid: u32) -> Result<Option<u32>> {
    valid(pid).map_or(Ok(None), platform::foreground_group)
}

/// `pid` when it can name a process other than the kernel's.
fn valid(pid: u32) -> Option<u32> {
    (pid > 0 && pid <= i32::MAX as u32).then_some(pid)
}

#[cfg(test)]
mod tests;
