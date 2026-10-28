//! What wakes a queued restore: sustained healthy readings for an agent
//! protection stopped mid-turn, an update for one stopped between turns,
//! which has nothing to do until then, unless `shoal resume` asked for it.
use std::collections::HashSet;

use anyhow::Result;
use rusqlite::Connection;

use super::{Wake, Work};
use crate::{
    daemon::workspace::Manager,
    execution::recovery,
    state::{QueueKind, QueueState},
};

/// What admitted a restore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Grant {
    /// Healthy readings, holding the recovery epoch a stop, reload or unsafe
    /// reading before the agent starts advances.
    Recovery(u64),
    /// An update for the agent.
    Update,
    /// Its turn in the agent queue, or `shoal queue start`.
    Queue,
}

/// A restore entry with the workspace and execution of its agent.
struct RestoreRow {
    id: String,
    workspace: String,
    execution: String,
}

/// Every restore entry of the queue that waits for an update when its
/// agent stopped between turns: one a waiting wrapper carries out, or one
/// Shoal queued on its own.
fn restore_rows(db: &Connection) -> Result<Vec<RestoreRow>> {
    Ok(db
        .prepare(
            "SELECT id,workspace_id,execution_id FROM agent_queue
             WHERE kind=?1 AND workspace_id IS NOT NULL AND execution_id IS NOT NULL
                AND (env IS NULL OR automatic)",
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

/// The executions of the workspace's restores that wait in the queue.
fn queued_restores(db: &Connection, workspace_id: &str) -> Result<Vec<String>> {
    Ok(db
        .prepare(
            "SELECT execution_id FROM agent_queue
             WHERE kind=?1 AND workspace_id=?2 AND state=?3 AND execution_id IS NOT NULL",
        )?
        .query_map(
            rusqlite::params![QueueKind::Restore, workspace_id, QueueState::Queued],
            |row| row.get(0),
        )?
        .collect::<rusqlite::Result<_>>()?)
}

impl Manager {
    /// The executions of the workspace whose wrapper waits, with every
    /// process of its agent stopped between turns, for an update that has
    /// not arrived.
    pub(crate) async fn idle_restores(&self, workspace_id: &str) -> Result<HashSet<String>> {
        let id = workspace_id.to_owned();
        let queued = self.store.run(move |db| queued_restores(db, &id)).await?;
        let waiters = self.agent_queue.restore_waiters();
        let mut idle = HashSet::new();
        for execution in queued {
            if waiters.contains(&execution)
                && self.restore_wake(workspace_id, &execution, true).await?
                    == (Wake::Update { due: false })
            {
                idle.insert(execution);
            }
        }
        Ok(idle)
    }

    /// The IDs of the restore entries that wait for an update, whose agent
    /// stopped between turns.
    pub(super) async fn restores_waiting_for_updates(&self) -> Result<HashSet<String>> {
        let restores = self.store.run(|db| restore_rows(db)).await?;
        let mut waiting = HashSet::new();
        for row in restores {
            if self.restore_waits_for_update(&row.workspace, &row.execution)
                && !self.moved_in_queue(&row.execution).await
            {
                waiting.insert(row.id);
            }
        }
        Ok(waiting)
    }

    /// What the queued restore of `execution` in `workspace_id` waits for:
    /// a slot after `shoal queue move`, otherwise what its recovery record
    /// says. Whether an update is due only matters once its agent `waits`,
    /// and free disk space must have recovered as for any other restore,
    /// since disk protection stops agents that start below it.
    pub(super) async fn restore_wake(
        &self,
        workspace_id: &str,
        execution: &str,
        waits: bool,
    ) -> Result<Wake> {
        if self.moved_in_queue(execution).await {
            return Ok(Wake::Slot);
        }
        if !self.restore_waits_for_update(workspace_id, execution) {
            return Ok(Wake::Recovery);
        }
        let due = waits && self.update_due(workspace_id).await?;
        Ok(Wake::Update { due })
    }

    /// How admission sees the restore of `execution` in `workspace_id` that
    /// the daemon starts: like a launch, except that an `automatic` one of
    /// an agent stopped between turns waits for an update first. Its agent's
    /// processes have stopped, so it holds no slot meanwhile.
    pub(super) async fn stopped_restore_work(
        &self,
        workspace_id: &str,
        execution: &str,
        automatic: bool,
    ) -> Result<Work> {
        if !automatic || !self.restore_waits_for_update(workspace_id, execution) {
            return Ok(Work::Launch);
        }
        let due = self.update_due(workspace_id).await?;
        Ok(Work::Restore {
            waiting: true,
            wake: Wake::Update { due },
        })
    }

    /// Whether an update waits for the agent of `workspace_id` and free disk
    /// space allows restoring it.
    async fn update_due(&self, workspace_id: &str) -> Result<bool> {
        Ok(self.restore_disk_ready() && self.updates_waiting(workspace_id).await?)
    }

    /// Whether disk space allows a restore an update woke: with disk
    /// protection on, the latest reading found the cleanup threshold free.
    fn restore_disk_ready(&self) -> bool {
        !self.config().overload.disk.enabled || self.config_state.disk_space_recovered()
    }

    /// Whether `shoal queue move` stopped the agent of `execution`.
    async fn moved_in_queue(&self, execution: &str) -> bool {
        self.agent_overload(execution)
            .await
            .is_some_and(|protection| protection.is_queue_move())
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
