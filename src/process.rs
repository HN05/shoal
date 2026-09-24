pub mod execution;
pub mod identity;

use anyhow::{Context, Result};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::{process::Command, time::timeout};

/// Automatic cleanup treats every process using the directory as activity.
pub async fn in_directory(root: &Path) -> Result<Vec<String>> {
    let excluded = [std::process::id()];
    let output = timeout(
        Duration::from_secs(10),
        Command::new("lsof")
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
            .current_dir("/")
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("process inspection timed out")?
    .context(
        "inspect workspace processes; install lsof and make it available on the daemon's PATH",
    )?;
    anyhow::ensure!(
        output.status.success(),
        "lsof could not inspect workspace processes: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let root = std::fs::canonicalize(root)?;
    let mut pid = 0;
    let mut name = String::new();
    let mut found = Vec::new();
    for line in String::from_utf8(output.stdout)?.lines() {
        if let Some(value) = line.strip_prefix('p') {
            pid = value.parse().context("invalid lsof PID")?;
        } else if let Some(value) = line.strip_prefix('c') {
            name = value.to_owned();
        } else if let Some(value) = line.strip_prefix('n') {
            if !excluded.contains(&pid) && Path::new(value).starts_with(&root) {
                found.push(format!("{name} (PID {pid})"));
            }
        }
    }
    Ok(found)
}
