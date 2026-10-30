//! Recording the copies of files attached to messages before Shoal recorded
//! attached files, so `shoal files` lists and deletes them too. The upgrade
//! marks every repository; the cleanup sweep records each one's copies once
//! no move of its directory is unfinished, which recovery finishes first.
use std::path::PathBuf;

use anyhow::{Context as _, Result, ensure};

use super::{FileSource, layout::Layout, records};
use crate::daemon::workspace::Manager;

impl Manager {
    /// Record the earlier copies of every marked repository whose directory
    /// is not moving. A failure keeps its mark for the next pass and does not
    /// skip the rest.
    pub async fn record_earlier_files(&self) -> Result<()> {
        let mut failed = Vec::new();
        for repository_id in self.store.run(|db| records::earlier_pending(db)).await? {
            if let Err(error) = self.record_repository_earlier_files(&repository_id).await {
                failed.push(format!("{repository_id}: {error:#}"));
            }
        }
        ensure!(failed.is_empty(), "{}", failed.join("; "));
        Ok(())
    }

    async fn record_repository_earlier_files(&self, repository_id: &str) -> Result<()> {
        let id = repository_id.to_owned();
        if self.store.run(move |db| records::moving(db, &id)).await? {
            return Ok(());
        }
        // A repository removed since has nothing left to record.
        if self.repository(repository_id).await.is_ok() {
            let (repo, _files) = self.lock_repository_files(repository_id).await?;
            if let Some(layout) = Layout::of(&repo) {
                self.record_layout_earlier_files(layout, repository_id)
                    .await?;
            }
        }
        let id = repository_id.to_owned();
        self.store
            .run(move |db| records::earlier_recorded(db, &id))
            .await
    }

    /// Record the earlier copies in each of the repository's workspaces.
    /// Callers hold the gate of its files.
    async fn record_layout_earlier_files(&self, layout: Layout, repository_id: &str) -> Result<()> {
        let id = repository_id.to_owned();
        let workspaces = self
            .store
            .run(move |db| records::repository_workspaces(db, &id))
            .await?;
        for workspace_id in workspaces {
            let (layout, id) = (layout.clone(), workspace_id.clone());
            let (copies, bytes) = tokio::task::spawn_blocking(move || -> Result<_> {
                let copies = layout.workspace_copies(&id)?;
                let bytes = sizes(&copies)?;
                Ok((copies, bytes))
            })
            .await??;
            self.store
                .run(move |db| {
                    records::record(
                        db,
                        &workspace_id,
                        FileSource::Message,
                        None,
                        &copies,
                        &bytes,
                    )
                })
                .await?;
        }
        Ok(())
    }
}

/// The size of each of `copies`.
fn sizes(copies: &[PathBuf]) -> Result<Vec<u64>> {
    copies
        .iter()
        .map(|copy| {
            let metadata = copy
                .metadata()
                .with_context(|| format!("read {}", copy.display()))?;
            Ok(metadata.len())
        })
        .collect()
}
