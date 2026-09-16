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
    repo: &Path,
    config: &Path,
    path: &Path,
    branch: &str,
    base: &str,
) -> Result<()> {
    let literal = serde_json::to_string(path.to_str().context("workspace path is not UTF-8")?)?;
    let mut command = Command::new("wt");
    command
        .arg("--config")
        .arg(config)
        .arg("--config-set")
        .arg(format!("worktree-path = {literal}"))
        .arg("-C")
        .arg(repo)
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
        std::fs::canonicalize(reported)? == std::fs::canonicalize(path)?,
        "Worktrunk created an unexpected workspace path"
    );
    Ok(())
}

pub async fn remove(repo: &Path, config: &Path, path: &Path) -> Result<()> {
    let mut command = Command::new("wt");
    command
        .arg("--config")
        .arg(config)
        .arg("-C")
        .arg(repo)
        .args([
            "remove",
            "--foreground",
            "--no-hooks",
            "--no-delete-branch",
            "--format=json",
            "--",
        ])
        .arg(path);
    let _: Value =
        serde_json::from_str(&run(command).await?).context("invalid Worktrunk removal result")?;
    ensure!(
        !path.exists(),
        "Worktrunk returned before workspace removal completed"
    );
    Ok(())
}
