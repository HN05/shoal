//! Ignored-file copies through Worktrunk (`wt`).
use crate::tools::Tool;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;
use tokio::process::Command;

use crate::subprocess;

#[cfg(test)]
mod tests;

/// Fields consumed from Worktrunk 0.81.0's `step copy-ignored` JSON result.
#[derive(Deserialize)]
struct CopyOutput {
    files: u64,
}

fn decode_copy(output: &str) -> Result<u64> {
    let result: CopyOutput =
        serde_json::from_str(output).context("invalid Worktrunk copy result")?;
    Ok(result.files)
}

/// Copy ignored files from the primary worktree into `workspace_dir`,
/// skipping files that already exist there, and return how many it copied.
pub async fn copy_ignored(worktrunk_config: &Path, workspace_dir: &Path) -> Result<u64> {
    let mut command = Command::new(Tool::Worktrunk.program());
    command.arg("--config").arg(worktrunk_config);
    command.arg("-C").arg(workspace_dir);
    command.args(["step", "copy-ignored", "--format=json"]);
    decode_copy(&subprocess::output(command).await?)
}
