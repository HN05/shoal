#[cfg(target_os = "macos")]
pub mod code_signing;
pub mod described;
mod directories;
pub mod execution;
pub mod identity;
pub mod load;
pub mod machine;
pub mod responsibility;
pub mod scoped;
pub mod tty;
pub mod usage;

use anyhow::{Context, Result};
use directories::WorkingDirectory;
use identity::Identity;
use std::{path::Path, time::Duration};

/// How long [`in_directory_besides`] waits for the Git commands this
/// process runs in the directory to end before it counts them.
const OWN_GIT_END: Duration = Duration::from_secs(60);

/// Automatic cleanup treats every process using the directory as activity.
pub async fn in_directory(root: &Path) -> Result<Vec<String>> {
    in_directory_except(root, &[]).await
}

/// The processes using the directory besides those of `excluded`.
pub async fn in_directory_except(root: &Path, excluded: &[u32]) -> Result<Vec<String>> {
    DirectoryUsers::default()
        .in_directory_except(root, excluded)
        .await
}

/// The processes using the directory other than those `excluded` names,
/// which is read after they are listed: a process started meanwhile that
/// it covers is among them, and one known to have exited since no longer
/// uses the directory. The Git commands this process runs there end first.
pub async fn in_directory_besides(
    root: &Path,
    excluded: impl Future<Output = Result<Vec<u32>>>,
) -> Result<Vec<String>> {
    let users = users_after_own_git(root).await?;
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

/// The processes using the directory once the Git commands this process
/// runs there, such as the daemon's status reads, and what they started
/// have ended; those still running after [`OWN_GIT_END`] count.
async fn users_after_own_git(root: &Path) -> Result<Vec<(u32, String)>> {
    let deadline = tokio::time::Instant::now() + OWN_GIT_END;
    loop {
        let users = DirectoryUsers::default();
        let found = users.of(root).await?;
        let own = users.own_git(&found).await?;
        if own.is_empty() || tokio::time::Instant::now() >= deadline {
            return Ok(found);
        }
        while tokio::time::Instant::now() < deadline
            && own
                .iter()
                .any(|process| identity::alive(process).unwrap_or(true))
        {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

/// The working directories of this user's processes other than this one,
/// listed on the first read and reused by every later one, so a sweep over
/// many directories scans once.
#[derive(Default)]
pub struct DirectoryUsers {
    listed: tokio::sync::OnceCell<Vec<WorkingDirectory>>,
}

impl DirectoryUsers {
    /// The processes using the directory besides those of `excluded`.
    pub async fn in_directory_except(&self, root: &Path, excluded: &[u32]) -> Result<Vec<String>> {
        Ok(self
            .of(root)
            .await?
            .into_iter()
            .filter(|(pid, _)| !excluded.contains(pid))
            .map(|(pid, name)| format!("{name} (PID {pid})"))
            .collect())
    }

    /// The processes whose working directory is in the directory, with
    /// their command names.
    async fn of(&self, root: &Path) -> Result<Vec<(u32, String)>> {
        let listed = self.listed.get_or_try_init(working_directories).await?;
        let root = std::fs::canonicalize(root)?;
        Ok(listed
            .iter()
            .filter(|process| process.path.starts_with(&root))
            .map(|process| (process.pid, process.name.clone()))
            .collect())
    }

    /// Those of `users` that are Git commands this process runs, or that
    /// such a command started.
    async fn own_git(&self, users: &[(u32, String)]) -> Result<Vec<Identity>> {
        let listed = self.listed.get_or_try_init(working_directories).await?;
        let mut own = Vec::new();
        for (pid, _) in users {
            let Some(child) = identity::own_child_above(*pid) else {
                continue;
            };
            let git = listed
                .iter()
                .any(|process| process.pid == child && process.name == "git");
            if git && let Some(identity) = identity::capture(*pid)? {
                own.push(identity);
            }
        }
        Ok(own)
    }
}

/// The working directory of each of this user's processes other than this
/// one, with their command names.
async fn working_directories() -> Result<Vec<WorkingDirectory>> {
    tokio::task::spawn_blocking(directories::working_directories)
        .await
        .context("working directory listing worker failed")?
}

#[cfg(test)]
mod tests;
