use anyhow::{Context, Result};
use std::{collections::HashMap, path::Path, process::Stdio, time::Duration};
use tokio::{process::Command, time::timeout};

use crate::worktrunk;

/// Exclude the requesting CLI and its ancestor shells for explicit removal.
/// Automatic cleanup passes zero and treats open shells as workspace users.
pub async fn in_directory(root: &Path, caller_pid: u32) -> Result<Vec<String>> {
    let mut excluded = vec![std::process::id()];
    if caller_pid != 0 {
        let mut command = Command::new("ps");
        command.args(["-axo", "pid=,ppid="]).current_dir("/");
        let output = worktrunk::run(command).await?;
        let parents: HashMap<u32, u32> = output
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
            })
            .collect();
        let mut pid = caller_pid;
        while pid != 0 && !excluded.contains(&pid) {
            excluded.push(pid);
            pid = parents.get(&pid).copied().unwrap_or(0);
        }
    }
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
