//! A shared repository's local config as the hub's copies of it replace
//! it, as the shared files are replaced.
use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use shoal_core::{config_file::copies, fsutil::off_runtime};
use shoal_ids::repository_ids::RepositoryId;

use crate::{Repositories, daemon::Daemon};

impl Repositories {
    /// The text of the repository `id`'s local config, none without one.
    pub async fn local_config_text(&self, id: &RepositoryId) -> Result<Option<String>> {
        let path = self.local_config_path(id).await?;
        off_runtime::run(move || {
            shoal_core::fsutil::read_optional(&path)
                .with_context(|| format!("read {}", path.display()))
        })
        .await
    }

    /// Make the repository `id`'s local config read `text`, the hub's copy
    /// of it, removing it for none, unless it no longer reads `expected`.
    /// With `keep`, the file it replaces stays as `config.toml.backup`,
    /// which is returned once there was a file to keep. It holds the gate
    /// and lock key edits take.
    pub async fn take_local_config_copy(
        &self,
        daemon: &dyn Daemon,
        id: &RepositoryId,
        expected: Option<String>,
        text: Option<String>,
        keep: bool,
    ) -> Result<Option<PathBuf>> {
        let _registry = self.gates.lock_registry().await;
        self.ensure_available(id).await?;
        let path = self.local_config_path(id).await?;
        let kept = off_runtime::run(move || {
            let held = copies::hold(&path)?;
            let current = shoal_core::fsutil::read_optional(&path)
                .with_context(|| format!("read {}", path.display()))?;
            ensure!(
                current == expected,
                "{} changed before the hub's copy could replace it",
                path.display()
            );
            held.write(&path, text.as_deref(), keep)
        })
        .await?;
        daemon.local_config_changed();
        Ok(kept)
    }
}

#[cfg(test)]
mod tests;
