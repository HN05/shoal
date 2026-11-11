//! What the kernel shows about a process beyond its identity: the command
//! line a person recognizes it by, and how long it has run. A process that
//! has exited, or whose records the kernel refuses, shows none of it.
use super::platform;
use anyhow::Result;
use std::{ffi::OsString, os::unix::ffi::OsStringExt, time::Duration};

/// The arguments `pid` runs with.
pub(crate) fn arguments(pid: u32) -> Option<Vec<OsString>> {
    let arguments = platform::arguments(valid(pid)?)?;
    Some(arguments.into_iter().map(OsString::from_vec).collect())
}

/// How long `pid` has run.
pub(crate) fn elapsed(pid: u32) -> Result<Option<Duration>> {
    valid(pid).map_or(Ok(None), platform::elapsed)
}

/// `pid` when it can name a process other than the kernel's.
fn valid(pid: u32) -> Option<u32> {
    (pid > 0 && pid <= i32::MAX as u32).then_some(pid)
}

#[cfg(test)]
mod tests;
