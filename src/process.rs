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
use std::path::Path;

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
/// uses the directory.
pub async fn in_directory_besides(
    root: &Path,
    excluded: impl Future<Output = Result<Vec<u32>>>,
) -> Result<Vec<String>> {
    let users = DirectoryUsers::default();
    let users = users.of(root).await?;
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
