//! The group a host's config names for a start, found by its label or
//! created, once however many Shoal processes of a state directory look for
//! it together: hosts find a group by listing and create it when none is
//! listed, so starts that list before either creates would each create one.
use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};
use sha2::{Digest as _, Sha256};

use super::Configured;
use crate::sessions::Origin;

/// The directory of the state directory that holds the groups' locks.
const DIRECTORY: &str = "terminal-groups";

/// The group labelled `label` in `host`, on the server of `near` or this
/// machine's, created at `cwd` when there is none; `None` for a host without
/// groups. Starts of the state directory `state` look for one host's group
/// of one label one at a time.
pub async fn labelled_group(
    state: &Path,
    host: &Configured,
    near: Option<&Origin>,
    label: &str,
    cwd: &Path,
) -> Result<Option<Origin>> {
    let Some(places) = host.host.places() else {
        return Ok(None);
    };
    let _lock = lock(lock_path(state, &host.host.name().to_string(), label)).await?;
    places.group(near, label, cwd).await
}

/// The lock of `host`'s groups labelled `label`, named by a digest since a
/// label is free text.
fn lock_path(state: &Path, host: &str, label: &str) -> PathBuf {
    let digest: String = Sha256::digest(format!("{host}\0{label}"))
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    state.join(DIRECTORY).join(format!("{digest}.lock"))
}

/// The lock at `path`, held until the file is dropped. Never unlinked:
/// another process may already have opened the same inode.
async fn lock(path: PathBuf) -> Result<File> {
    tokio::task::spawn_blocking(move || {
        let directory = path.parent().context("a group lock has no directory")?;
        std::fs::create_dir_all(directory)
            .with_context(|| format!("create {}", directory.display()))?;
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("open terminal group lock {}", path.display()))?;
        fs2::FileExt::lock_exclusive(&file)
            .with_context(|| format!("lock terminal group lock {}", path.display()))?;
        Ok(file)
    })
    .await?
}

#[cfg(test)]
mod tests;
