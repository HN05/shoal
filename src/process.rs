#[cfg(target_os = "macos")]
pub mod code_signing;
pub mod described;
pub mod execution;
pub mod identity;
pub mod load;
pub mod machine;
pub mod responsibility;
pub mod scoped;
pub mod tty;
pub mod usage;

use crate::tools::Tool;
use anyhow::{Context, Result};
use std::{path::Path, time::Duration};
use tokio::process::Command;

/// Automatic cleanup treats every process using the directory as activity.
pub async fn in_directory(root: &Path) -> Result<Vec<String>> {
    in_directory_except(root, &[]).await
}

/// The processes using the directory besides those of `excluded`.
pub async fn in_directory_except(root: &Path, excluded: &[u32]) -> Result<Vec<String>> {
    Ok(directory_users(root)
        .await?
        .into_iter()
        .filter(|(pid, _)| !excluded.contains(pid))
        .map(|(pid, name)| format!("{name} (PID {pid})"))
        .collect())
}

/// The processes using the directory other than those `excluded` names,
/// which is read after they are listed: a process started meanwhile that
/// it covers is among them, and one known to have exited since no longer
/// uses the directory.
pub async fn in_directory_besides(
    root: &Path,
    excluded: impl Future<Output = Result<Vec<u32>>>,
) -> Result<Vec<String>> {
    let users = directory_users(root).await?;
    let excluded = excluded.await?;
    let mut found = Vec::new();
    for (pid, name) in users {
        if excluded.contains(&pid) || identity::capture(pid)?.is_none() {
            continue;
        }
        found.push(format!("{name} (PID {pid})"));
    }
    Ok(found)
}

/// This user's processes whose working directory is in the directory, other
/// than this one, with their command names.
async fn directory_users(root: &Path) -> Result<Vec<(u32, String)>> {
    let own = std::process::id();
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
            && pid != own
            && Path::new(value).starts_with(&root)
        {
            found.push((pid, name.clone()));
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests;
