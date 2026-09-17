//! Worktree creation and removal through Worktrunk (`wt`).
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::path::Path;
use tokio::process::Command;

use crate::{removal::RemovalResult, subprocess};

fn command(repository_dir: &Path, worktrunk_config: &Path) -> Command {
    let mut command = Command::new("wt");
    command.arg("--config").arg(worktrunk_config);
    command.arg("-C").arg(repository_dir);
    command
}

pub async fn create(
    repository_dir: &Path,
    worktrunk_config: &Path,
    workspace_dir: &Path,
    branch: &str,
    base: &str,
) -> Result<()> {
    let literal = serde_json::to_string(
        workspace_dir
            .to_str()
            .context("workspace path is not UTF-8")?,
    )?;
    let mut command = Command::new("wt");
    command
        .arg("--config")
        .arg(worktrunk_config)
        .arg("--config-set")
        .arg(format!("worktree-path = {literal}"))
        .arg("-C")
        .arg(repository_dir)
        .args([
            "switch",
            "--create",
            branch,
            "--base",
            base,
            "--no-cd",
            "--no-hooks",
            "--format=json",
        ]);
    let result: Value = serde_json::from_str(&subprocess::output(command).await?)
        .context("invalid Worktrunk creation result")?;
    ensure!(
        result["action"] == "created",
        "Worktrunk did not create a new workspace: {result}"
    );
    let reported = result["path"]
        .as_str()
        .context("Worktrunk omitted workspace path")?;
    ensure!(
        std::fs::canonicalize(reported)? == std::fs::canonicalize(workspace_dir)?,
        "Worktrunk created an unexpected workspace path"
    );
    Ok(())
}

pub async fn remove(
    repository_dir: &Path,
    worktrunk_config: &Path,
    workspace_dir: &Path,
    force_files: bool,
    delete_branch: bool,
) -> Result<RemovalResult> {
    let mut command = command(repository_dir, worktrunk_config);
    command.args(["remove", "--foreground", "--no-hooks", "--format=json"]);
    command.arg(if delete_branch {
        "--force-delete"
    } else {
        "--no-delete-branch"
    });
    if force_files {
        command.arg("--force");
    }
    command.arg("--").arg(workspace_dir);
    let result: Value = serde_json::from_str(&subprocess::output(command).await?)
        .context("invalid Worktrunk removal result")?;
    let result = match result.as_array() {
        Some(entries) => {
            ensure!(
                entries.len() == 1,
                "unexpected number of Worktrunk removal results"
            );
            &entries[0]
        }
        None => &result,
    };
    ensure!(
        !workspace_dir.exists(),
        "Worktrunk returned before workspace removal completed"
    );
    let branch_outcome = result["branch_outcome"]
        .as_str()
        .context("Worktrunk omitted branch outcome")?
        .to_owned();
    Ok(RemovalResult {
        removed: true,
        branch: result["branch"].as_str().map(str::to_owned),
        branch_deleted: branch_outcome == "deleted",
        branch_outcome,
    })
}
