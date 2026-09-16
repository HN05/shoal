use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::{path::Path, process::Stdio};
use tokio::process::Command;

pub async fn run(mut command: Command) -> Result<String> {
    let program = command
        .as_std()
        .get_program()
        .to_string_lossy()
        .into_owned();
    let output = command
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .with_context(|| {
            format!("run {program}; ensure it is installed and on the daemon's PATH")
        })?;
    if !output.status.success() {
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        // Keep errors within the control protocol frame limit.
        bail!(
            "{program} failed ({}): {}",
            output.status,
            diagnostic.chars().take(8192).collect::<String>()
        );
    }
    String::from_utf8(output.stdout).context("tool output is not UTF-8")
}

pub async fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let mut command = Command::new("git");
    command.arg("-C").arg(repo).args(args);
    run(command).await
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
    let result: Value =
        serde_json::from_str(&run(command).await?).context("invalid Worktrunk creation result")?;
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
    confirmed: bool,
) -> Result<()> {
    let mut command = Command::new("wt");
    command
        .arg("--config")
        .arg(worktrunk_config)
        .arg("-C")
        .arg(repository_dir)
        .args([
            "remove",
            "--foreground",
            "--no-hooks",
            "--no-delete-branch",
            "--format=json",
        ]);
    if confirmed {
        command.arg("--force");
    }
    command.arg("--").arg(workspace_dir);
    let _: Value =
        serde_json::from_str(&run(command).await?).context("invalid Worktrunk removal result")?;
    ensure!(
        !workspace_dir.exists(),
        "Worktrunk returned before workspace removal completed"
    );
    Ok(())
}
