//! What wakes a queued restore: sustained healthy readings for an agent
//! stopped mid-turn, an update for one stopped between turns, which has
//! nothing to do until then.
use anyhow::Result;

use super::Wake;
use crate::{daemon::workspace::Manager, execution::recovery};

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
    /// What the queued restore of `execution` in `workspace_id` waits for,
    /// by its recovery record; whether an update is due only matters once
    /// its agent `waits`.
    pub(super) async fn restore_wake(
        &self,
        workspace_id: &str,
        execution: &str,
        waits: bool,
    ) -> Result<Wake> {
        let path = recovery::record_path(&self.paths, workspace_id, execution);
        // An unreadable record leaves the turn unknown, which counts as mid-turn.
        let between_turns = recovery::Record::load(&path)
            .ok()
            .flatten()
            .is_some_and(|record| record.between_turns());
        if !between_turns {
            return Ok(Wake::Recovery);
        }
        let due = waits && self.updates_waiting(workspace_id).await?;
        Ok(Wake::Update { due })
    }
}
