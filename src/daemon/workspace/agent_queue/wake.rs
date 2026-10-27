//! What wakes a queued restore: sustained healthy readings for an agent
//! stopped mid-turn, an update for one stopped between turns, which has
//! nothing to do until then.
use std::collections::HashSet;

use anyhow::Result;
use rusqlite::Connection;

use super::Wake;
use crate::{daemon::workspace::Manager, execution::recovery, state::QueueKind};

/// What admitted a restore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Grant {
    /// Healthy readings, holding the recovery epoch a stop, reload or unsafe
    /// reading before the agent starts advances.
    Recovery(u64),
    /// An update for the agent.
    Update,
}

/// A restore entry with the workspace and execution of its agent.
struct RestoreRow {
    id: String,
    workspace: String,
    execution: String,
}

/// Every restore entry of the queue.
fn restore_rows(db: &Connection) -> Result<Vec<RestoreRow>> {
    Ok(db
        .prepare(
            "SELECT id,workspace_id,execution_id FROM agent_queue
             WHERE kind=?1 AND workspace_id IS NOT NULL AND execution_id IS NOT NULL",
        )?
        .query_map([QueueKind::Restore], |row| {
            Ok(RestoreRow {
                id: row.get(0)?,
                workspace: row.get(1)?,
                execution: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

impl Manager {
    /// The IDs of the restore entries whose agent stopped between turns.
    pub(super) async fn restores_waiting_for_updates(&self) -> Result<HashSet<String>> {
        let restores = self.store.run(|db| restore_rows(db)).await?;
        Ok(restores
            .into_iter()
            .filter(|row| self.restore_waits_for_update(&row.workspace, &row.execution))
            .map(|row| row.id)
            .collect())
    }

    /// What the queued restore of `execution` in `workspace_id` waits for,
    /// by its recovery record. Whether an update is due only matters once
    /// its agent `waits`, and free disk space must have recovered as for any
    /// other restore, since disk protection stops agents that start below it.
    pub(super) async fn restore_wake(
        &self,
        workspace_id: &str,
        execution: &str,
        waits: bool,
    ) -> Result<Wake> {
        if !self.restore_waits_for_update(workspace_id, execution) {
            return Ok(Wake::Recovery);
        }
        let due = waits && self.restore_disk_ready() && self.updates_waiting(workspace_id).await?;
        Ok(Wake::Update { due })
    }

    /// Whether disk space allows a restore an update woke: with disk
    /// protection on, the latest reading found the cleanup threshold free.
    fn restore_disk_ready(&self) -> bool {
        !self.config().overload.disk.enabled || self.disk_space_recovered()
    }

    /// Whether the recovery record of `execution` says it stopped between
    /// turns, so its restore waits for an update. An unreadable record leaves
    /// the turn unknown, which counts as mid-turn.
    pub(crate) fn restore_waits_for_update(&self, workspace_id: &str, execution: &str) -> bool {
        let path = recovery::record_path(&self.paths, workspace_id, execution);
        recovery::Record::load(&path)
            .ok()
            .flatten()
            .is_some_and(|record| record.between_turns())
    }
}
