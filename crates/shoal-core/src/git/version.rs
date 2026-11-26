//! The Git release a command needs, named in its failure only when the Git
//! that ran could be the cause.
use std::{io, process::Output};

use anyhow::Result;
use tokio::process::Command;

use crate::{subprocess, tools::Tool};

pub(crate) type Release = (u32, u32);

/// Git's exit status for arguments it does not take, as an older Git exits
/// for an option it lacks.
const USAGE: i32 = 129;

/// The stdout of `output`, a run of `program` that needs Git `needed`,
/// checked as [`subprocess::checked_output`] does. A failure says that Git
/// `needed` or newer is required only when Git refused the arguments or the
/// installed Git is older; any other failure is Git's own.
pub(crate) async fn checked_output(
    program: &str,
    output: io::Result<Output>,
    needed: Release,
) -> Result<String> {
    let refused = output
        .as_ref()
        .is_ok_and(|output| output.status.code() == Some(USAGE));
    let error = match subprocess::checked_output(program, output) {
        Ok(stdout) => return Ok(stdout),
        Err(error) => error,
    };
    if refused || installed().await.is_some_and(|found| found < needed) {
        let (major, minor) = needed;
        return Err(error.context(format!("Git {major}.{minor} or newer is required")));
    }
    Err(error)
}

async fn installed() -> Option<Release> {
    let mut command = Command::new(Tool::Git.program());
    command.args(["--no-optional-locks", "--version"]);
    parse(&super::output(command).await.ok()?)
}

/// The release `git --version` printed, such as `git version 2.39.5 (Apple
/// Git-154)`.
fn parse(version: &str) -> Option<Release> {
    let mut parts = version.trim().strip_prefix("git version ")?.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor: String = parts
        .next()?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    Some((major, minor.parse().ok()?))
}

#[cfg(test)]
mod tests;
