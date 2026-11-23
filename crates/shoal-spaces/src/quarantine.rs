//! The operations a daemon left unfinished when it stopped, which the next
//! one quarantines at startup, while it holds the startup lock and before it
//! audits or serves anything.
use std::sync::Arc;

use anyhow::{Context, Result};

use super::store::Store;
use crate::Spaces;
use crate::daemon::Records;
use crate::state::WorkspaceState;

impl Spaces {
    /// Mark the operations a stopped daemon left running as interrupted, in
    /// one transaction of its own, after the store's migration.
    pub async fn interrupted_operations(&self) -> Result<()> {
        interrupted_operations(&self.store, self.records.clone()).await
    }
}

/// Mark the operations a stopped daemon left running in `store` as
/// interrupted, those other subsystems record through `records`, in one
/// transaction of its own.
pub async fn interrupted_operations(store: &Store, records: Arc<dyn Records>) -> Result<()> {
    store
        .write(move |tx| {
            records.interrupt_operations(tx)?;
            super::lifecycle::fail_interrupted_removals(tx)?;
            tx.execute(
                "UPDATE workspaces SET state=?1, error='daemon stopped during workspace operation; inspect before cleanup'
                    WHERE state IN (SELECT value FROM json_each(?2))
                        AND id NOT IN (SELECT workspace_id FROM waiting_preparations)",
                rusqlite::params![
                    WorkspaceState::Failed,
                    serde_json::to_string(&WorkspaceState::transient().collect::<Vec<_>>())?,
                ],
            )?;
            Ok(())
        })
        .await
        .context("quarantine interrupted operations at daemon startup")
}
