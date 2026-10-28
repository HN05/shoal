//! Subprocess calls to the per-user service manager.
use crate::{protocol::timing, tools::Tool};
use anyhow::{Context, Result, bail};
use std::time::Duration;
use tokio::process::Command;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(20);
/// Give the OS time to finish a daemon drain before forcing termination.
pub(super) const SERVICE_STOP_TIMEOUT: Duration =
    timing::WORKSPACE_STOP_TIMEOUT.saturating_add(COMMAND_TIMEOUT);
/// A synchronous stop must also allow the service manager to finish its job.
const STOP_COMMAND_TIMEOUT: Duration = SERVICE_STOP_TIMEOUT.saturating_add(COMMAND_TIMEOUT);

pub(super) async fn command(tool: Tool, args: &[&str], check: bool) -> Result<bool> {
    run(tool, args, check, COMMAND_TIMEOUT).await
}

pub(super) async fn stop_command(tool: Tool, args: &[&str]) -> Result<bool> {
    run(tool, args, true, STOP_COMMAND_TIMEOUT).await
}

async fn run(tool: Tool, args: &[&str], check: bool, limit: Duration) -> Result<bool> {
    let program = tool.program();
    let mut command = Command::new(program);
    command.args(args);
    let output = crate::subprocess::Run::new(command)
        .timeout(limit)
        .capture()
        .await
        .with_context(|| format!("run {program}; a working per-user service manager is required (use `shoal daemon run` otherwise)"))?;
    if check && !output.status.success() {
        bail!(
            "{program} {} failed: {}",
            args.join(" "),
            crate::subprocess::diagnostic(&output.stderr).trim()
        );
    }
    Ok(output.status.success())
}

pub(super) fn domain() -> String {
    // SAFETY: geteuid has no arguments or memory-safety preconditions.
    format!("gui/{}", unsafe { libc::geteuid() })
}

#[cfg(test)]
mod tests;
