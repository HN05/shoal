//! The operations a daemon left unfinished when it stopped, which the next
//! one quarantines at startup, while it holds the startup lock and before it
//! audits or serves anything.
use anyhow::{Context, Result};

use super::store::Store;
use crate::state::{ExecutionState, WorkspaceState};
use shoal_protocol::sim::CleanRequestStatus;

/// Mark the operations a stopped daemon left running in `store` as
/// interrupted, in one transaction of its own, after the store's migration.
pub(crate) async fn interrupted_operations(store: &Store) -> Result<()> {
    store
        .write(|tx| {
            tx.execute(
                "UPDATE simulator_clean_requests SET record=json_set(record, '$.status', ?1)
                    WHERE json_extract(record, '$.status')=?2",
                [CleanRequestStatus::Interrupted, CleanRequestStatus::Requested],
            )?;
            super::lifecycle::fail_interrupted_removals(tx)?;
            tx.execute(
                "UPDATE executions SET state=?1 WHERE state=?2",
                [ExecutionState::Unknown, ExecutionState::Running],
            )?;
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

#[cfg(test)]
mod tests;
