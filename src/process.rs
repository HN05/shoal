pub mod execution;
pub mod identity;

use crate::tools::Tool;
use anyhow::{Context, Result};
use std::{path::Path, time::Duration};
use tokio::process::Command;

/// Automatic cleanup treats every process using the directory as activity.
pub async fn in_directory(root: &Path) -> Result<Vec<String>> {
    let excluded = [std::process::id()];
    let mut command = Command::new(Tool::Lsof.program());
    command
        .args([
            "-n",
            "-P",
            "-a",
            "-u",
            &unsafe { libc::geteuid() }.to_string(),
            "-d",
            "cwd",
            "-Fpcn",
        ])
        .current_dir("/");
    let output = crate::subprocess::Run::new(command)
        .timeout(Duration::from_secs(10))
        .output()
        .await
        .context(
            "inspect workspace processes; install lsof and make it available on the daemon's PATH",
        )?;
    let root = std::fs::canonicalize(root)?;
    let mut pid = 0;
    let mut name = String::new();
    let mut found = Vec::new();
    for line in output.lines() {
        if let Some(value) = line.strip_prefix('p') {
            pid = value.parse().context("invalid lsof PID")?;
        } else if let Some(value) = line.strip_prefix('c') {
            name = value.to_owned();
        } else if let Some(value) = line.strip_prefix('n')
            && !excluded.contains(&pid)
            && Path::new(value).starts_with(&root)
        {
            found.push(format!("{name} (PID {pid})"));
        }
    }
    Ok(found)
}
