//! Cross-state coordination for hosts whose native close is not conditional.
use std::{
    fs::{File, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};

/// Hold the server's lock through native ownership reads and mutations. Never
/// unlink it: another process may already have opened the same inode.
pub(super) async fn for_server(socket: &Path) -> Result<File> {
    let path = lock_path(socket)?;
    tokio::task::spawn_blocking(move || {
        let file = open(&path)?;
        fs2::FileExt::lock_exclusive(&file)?;
        Ok(file)
    })
    .await?
}

fn lock_path(socket: &Path) -> Result<PathBuf> {
    let socket = match socket.canonicalize() {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let socket = std::path::absolute(socket)?;
            socket
                .parent()
                .context("terminal server socket has no parent")?
                .canonicalize()?
                .join(
                    socket
                        .file_name()
                        .context("terminal server socket has no name")?,
                )
        }
        Err(error) => return Err(error.into()),
    };
    let mut path = socket.into_os_string();
    path.push(".shoal-ownership.lock");
    Ok(path.into())
}

fn open(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    if crate::build::changes_user_files() {
        options.write(true).create(true).truncate(false).mode(0o600);
    }
    options.open(path).with_context(|| {
        if crate::build::changes_user_files() {
            format!("open terminal ownership lock {}", path.display())
        } else {
            format!(
                "open terminal ownership lock {}; development builds leave creating it to the installed Shoal or an isolated sandbox",
                path.display()
            )
        }
    })
}

#[cfg(test)]
mod tests;
