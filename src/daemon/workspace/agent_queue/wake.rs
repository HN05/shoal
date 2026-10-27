//! What wakes a queued restore: sustained healthy readings for an agent
//! stopped mid-turn, an update for one stopped between turns, which has
//! nothing to do until then.
use std::collections::HashSet;

use anyhow::Result;

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

impl Manager {
    /// The IDs of the restore entries whose agent stopped between turns.
    pub(super) async fn restores_waiting_for_updates(&self) -> Result<HashSet<String>> {
        let restores = self
            .store
            .run(|db| {
                Ok(db
                    .prepare(
                        "SELECT id,workspace_id,execution_id FROM agent_queue
                         WHERE kind=?1 AND workspace_id IS NOT NULL AND execution_id IS NOT NULL",
                    )?
                    .query_map([QueueKind::Restore], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                    })?
                    .collect::<rusqlite::Result<Vec<(String, String, String)>>>()?)
            })
            .await?;
        Ok(restores
            .into_iter()
            .filter(|(_, workspace, execution)| self.stopped_between_turns(workspace, execution))
            .map(|(id, _, _)| id)
            .collect())
    }

    /// What the queued restore of `execution` in `workspace_id` waits for,
    /// by its recovery record; whether an update is due only matters once
    /// its agent `waits`.
    pub(super) async fn restore_wake(
        &self,
        workspace_id: &str,
        execution: &str,
        waits: bool,
    ) -> Result<Wake> {
        if !self.stopped_between_turns(workspace_id, execution) {
            return Ok(Wake::Recovery);
        }
        let due = waits && self.updates_waiting(workspace_id).await?;
        Ok(Wake::Update { due })
    }

    /// Whether the recovery record of `execution` says it stopped between
    /// turns. An unreadable record leaves the turn unknown, which counts as
    /// mid-turn.
    fn stopped_between_turns(&self, workspace_id: &str, execution: &str) -> bool {
        let path = recovery::record_path(&self.paths, workspace_id, execution);
        recovery::Record::load(&path)
            .ok()
            .flatten()
            .is_some_and(|record| record.between_turns())
    }
}
