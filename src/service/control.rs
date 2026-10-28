//! Subprocess calls to the per-user service manager.
use crate::tools::Tool;
use anyhow::{Context, Result, bail};
use std::time::Duration;
use tokio::process::Command;

pub(super) async fn command(tool: Tool, args: &[&str], check: bool) -> Result<bool> {
    let program = tool.program();
    let mut command = Command::new(program);
    command.args(args);
    let output = crate::subprocess::Run::new(command)
        .timeout(Duration::from_secs(20))
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
