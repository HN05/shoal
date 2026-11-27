//! What wakes a queued restore, decided from its agent's recovery record
//! whatever carries the restore out. `docs/design/queue.md` says what each kind
//! of stop waits for.
use crate::execution::recovery::RecordExt;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::{queue_entry_ids::QueueEntryId, space_ids::SpaceId};
use std::collections::HashSet;

use anyhow::Result;
use rusqlite::Connection;

use super::limits::Wake;
use crate::Agents;
use crate::daemon::Daemon;
use crate::execution::recovery;
use crate::state::QueueKind;
use crate::state::QueueState;
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::time::UnixTime;

/// What admitted a restore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grant {
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
    id: QueueEntryId,
    workspace: SpaceId,
    execution: ExecutionId,
}

/// Every restore entry of the queue that waits for an update when its
/// agent stopped between turns: one Shoal queued on its own, for a waiting
/// wrapper or for the daemon to start.
fn restore_rows(db: &Connection) -> Result<Vec<RestoreRow>> {
    Ok(db
        .prepare(
            "SELECT id,space_id,execution_id FROM agent_queue
             WHERE kind=?1 AND space_id IS NOT NULL AND execution_id IS NOT NULL
                AND automatic",
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

/// A restore of the workspace that waits in the queue.
struct QueuedRestore {
    execution: ExecutionId,
    /// Whether the daemon starts it, rather than a waiting wrapper.
    started_here: bool,
    /// Whether Shoal queued it on its own.
    automatic: bool,
}

/// The workspace's restores that wait in the queue.
fn queued_restores(db: &Connection, workspace_id: &SpaceId) -> Result<Vec<QueuedRestore>> {
    Ok(db
        .prepare(
            "SELECT execution_id,env IS NOT NULL,automatic FROM agent_queue
             WHERE kind=?1 AND space_id=?2 AND state=?3 AND execution_id IS NOT NULL",
        )?
        .query_map(
            rusqlite::params![QueueKind::Restore, workspace_id, QueueState::Queued],
            |row| {
                Ok(QueuedRestore {
                    execution: row.get(0)?,
                    started_here: row.get(1)?,
                    automatic: row.get(2)?,
                })
            },
        )?
        .collect::<rusqlite::Result<_>>()?)
}

/// Why Shoal stopped an agent, as its recovery record says, which decides
/// what the agent's restore waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopped {
    /// Its space was blocked: the restore waits until nothing blocks it.
    Blocked,
    /// Between turns: the restore waits for an update or a message.
    BetweenTurns,
    /// Mid-turn for load or free disk space: the restore waits for healthy
    /// readings.
    ForLoad,
    /// By the OOM killer: the restore waits for healthy readings, for an
    /// update first when it was `between_turns`, and until `restores_after`
    /// after repeated kills.
    OomKilled {
        between_turns: bool,
        restores_after: Option<UnixTime>,
    },
    /// Mid-turn otherwise, such as by a signal or a shutdown, or with no
    /// readable record: the restore waits for a free slot.
    MidTurn,
}

impl Stopped {
    fn of(record: Option<&recovery::Record>) -> Self {
        match record {
            Some(record)
                if record.stop_reason.as_deref() == Some(crate::control::BLOCKED_REASON) =>
            {
                Self::Blocked
            }
            Some(record) if record.oom_kills > 0 => Self::OomKilled {
                between_turns: record.between_turns(),
                restores_after: record.restores_after,
            },
            Some(record) if record.between_turns() => Self::BetweenTurns,
            Some(record) if record.for_load => Self::ForLoad,
            Some(_) | None => Self::MidTurn,
        }
    }

    /// Whether its restore waits for healthy readings once nothing else
    /// holds it.
    pub fn waits_for_recovery(self) -> bool {
        matches!(self, Self::ForLoad | Self::OomKilled { .. })
    }

    /// Whether the agent stopped between turns, so that its restore waits
    /// for an update.
    fn between_turns(self) -> bool {
        matches!(
            self,
            Self::BetweenTurns
                | Self::OomKilled {
                    between_turns: true,
                    ..
                }
        )
    }

    /// Whether the restore waits for an update, or for nothing to block
    /// its space.
    fn waits_for_update(self) -> bool {
        self == Self::Blocked || self.between_turns()
    }
}

/// What the restore of an agent the OOM killer ended waits for: healthy
/// readings once nothing else `waits`, so that it holds no slot before.
fn oom_wake(waits: bool) -> Wake {
    match waits {
        true => Wake::Update { due: false },
        false => Wake::Recovery,
    }
}

impl Agents {
    /// The executions of the workspace whose agent, stopped between turns with
    /// every process, waits in the queue for an update while none is recorded
    /// for it and neither a timer nor a turn retry will wake it. Disk space may
    /// still hold back one that has an update, which has work all the same. An
    /// agent stopped because its space is blocked is not idle.
    pub async fn idle_restores(
        &self,
        daemon: &dyn Daemon,
        workspace_id: &SpaceId,
    ) -> Result<HashSet<ExecutionId>> {
        let id = workspace_id.to_owned();
        let queued = self.store.run(move |db| queued_restores(db, &id)).await?;
        let carriers = self.carriers();
        let mut idle: HashSet<ExecutionId> = (queued.into_iter())
            .filter(|restore| {
                restore.automatic
                    && carriers
                        .of(restore.started_here, &restore.execution)
                        .is_some()
                    && self
                        .stopped_as(workspace_id, &restore.execution)
                        .between_turns()
            })
            .map(|restore| restore.execution)
            .collect();
        if !idle.is_empty()
            && (daemon.agent_updates_recorded(workspace_id).await?
                || self.wake_timer_set(workspace_id).await?
                || self.waiting_turn_retry(workspace_id).await?.is_some())
        {
            idle.clear();
        }
        Ok(idle)
    }

    /// The IDs of the restore entries that wait for an update, whose agent
    /// stopped between turns.
    pub async fn restores_waiting_for_updates(&self) -> Result<HashSet<QueueEntryId>> {
        let restores = self.store.run(|db| restore_rows(db)).await?;
        Ok((restores.into_iter())
            .filter(|row| {
                self.stopped_as(&row.workspace, &row.execution)
                    .between_turns()
            })
            .map(|row| row.id)
            .collect())
    }

    /// What the queued restore of `execution` in the space `id` waits for,
    /// whatever carries it out: a slot once `shoal resume` asked for it,
    /// otherwise what its recovery record says. An update is due only once free
    /// disk space has recovered, since disk protection stops agents that start
    /// below it.
    pub async fn restore_wake(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        execution: &ExecutionId,
        automatic: bool,
    ) -> Result<Wake> {
        if !automatic {
            return Ok(Wake::Slot);
        }
        match self.stopped_as(id, execution) {
            Stopped::ForLoad => Ok(Wake::Recovery),
            Stopped::MidTurn => Ok(Wake::Slot),
            Stopped::Blocked | Stopped::BetweenTurns => Ok(Wake::Update {
                due: self.restore_due(daemon, id, execution).await?,
            }),
            Stopped::OomKilled {
                between_turns,
                restores_after,
            } => {
                let held = restores_after.is_some_and(|after| after > UnixTime::now());
                let waits =
                    held || (between_turns && !self.restore_due(daemon, id, execution).await?);
                Ok(oom_wake(waits))
            }
        }
    }

    /// Whether the restore of `execution`, stopped between turns in
    /// `workspace_id`, is due by its record, and free disk space allows it.
    pub async fn restore_due(
        &self,
        daemon: &dyn Daemon,
        workspace_id: &SpaceId,
        execution: &ExecutionId,
    ) -> Result<bool> {
        if !self.restore_disk_ready() {
            return Ok(false);
        }
        if self.restore_waits_for_unblock(workspace_id, execution) {
            return Ok(!daemon.space_blocked(workspace_id).await?);
        }
        daemon
            .updates_waiting(&SpaceSelector::id(workspace_id))
            .await
    }

    /// Whether disk space allows a restore an update woke: with disk
    /// protection on, the latest reading found the cleanup threshold free.
    fn restore_disk_ready(&self) -> bool {
        !self.config.current().machine.overload.disk.enabled || self.config.disk_space_recovered()
    }

    /// Why Shoal stopped the agent of `execution` in the space `id`, as its
    /// recovery record says, in a workspace or a chat alike.
    pub fn stopped_as(&self, id: &SpaceId, execution: &ExecutionId) -> Stopped {
        let path = recovery::record_path(&self.paths, id, execution);
        Stopped::of(recovery::Record::load(&path).ok().flatten().as_ref())
    }

    /// Whether the recovery record of `execution` says Shoal stopped it
    /// because its space was blocked, so its restore waits until nothing
    /// blocks the space.
    pub fn restore_waits_for_unblock(
        &self,
        workspace_id: &SpaceId,
        execution: &ExecutionId,
    ) -> bool {
        self.stopped_as(workspace_id, execution) == Stopped::Blocked
    }

    /// Whether the recovery record of `execution` in the space `id` says it
    /// stopped between turns, so its restore waits for an update, or for
    /// nothing to block its space.
    pub fn restore_waits_for_update(&self, id: &SpaceId, execution: &ExecutionId) -> bool {
        self.stopped_as(id, execution).waits_for_update()
    }
}
