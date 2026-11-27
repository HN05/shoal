//! The OOM kills of each space's agent since it last finished a turn, which
//! make its restores wait for memory to recover and back off as they repeat.
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;

use crate::execution::recovery::{self, OOM_REASON, RecordExt};
use crate::model::Asleep;
use crate::queue::stopped::WAKES_IN_QUEUE;
use crate::state::AgentState;
use crate::time::UnixTime;

/// The OOM kills of a space's agent since it last finished a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OomKills {
    pub kills: u32,
    pub killed_at: UnixTime,
}

impl OomKills {
    /// The one kill a stop's recovery record names that the store has yet to
    /// count, as when the OOM killer ended the agent while its wrapper could
    /// not reach the daemon. Being the first, it waits out no backoff.
    pub fn uncounted() -> Self {
        Self {
            kills: 1,
            killed_at: UnixTime::now(),
        }
    }

    /// When its restore may start: at once after the first kill, then after
    /// a delay that doubles with each kill before a finished turn.
    pub fn restores_after(self) -> UnixTime {
        self.killed_at + backoff(self.kills)
    }

    /// `asleep` as these kills make it, at `now`: named by its kills, and
    /// waking after its backoff while that lasts, and once memory recovers
    /// besides what it waited for while recovery's healthy `readings` are on.
    pub fn asleep(self, asleep: Asleep, now: UnixTime, readings: bool) -> Asleep {
        let times = match self.kills {
            1 => "once".to_owned(),
            kills => format!("{kills} times"),
        };
        let restores_after = self.restores_after();
        let after = match now < restores_after {
            true => format!(" after {}", restores_after.local_minutes()),
            false => String::new(),
        };
        let wakes_when = match (readings, asleep.wakes_when.as_str()) {
            (false, wakes) => format!("{wakes}{after}"),
            (true, WAKES_IN_QUEUE) => format!("memory recovers{after}"),
            (true, update) => format!("{update} and memory recovers{after}"),
        };
        Asleep {
            reason: format!("OOM-killed {times}"),
            wakes_when,
            ..asleep
        }
    }
}

/// How long the restore after the `kills`th kill waits: none after the
/// first, then 5, 10, 20 and 40 minutes, and 2 hours from then on.
fn backoff(kills: u32) -> Duration {
    const FIRST: Duration = Duration::from_secs(5 * 60);
    const LONGEST: Duration = Duration::from_secs(2 * 60 * 60);
    match kills {
        0 | 1 => Duration::ZERO,
        kills => FIRST.saturating_mul(1 << (kills - 2).min(5)).min(LONGEST),
    }
}

/// Count the OOM kill that ended `execution`, the agent of the space `id`, at
/// `at`, unless it was the last kill counted. Returns whether it counted it.
pub fn record(
    db: &Connection,
    id: &SpaceId,
    execution: &ExecutionId,
    at: UnixTime,
) -> Result<bool> {
    let counted = db.execute(
        "INSERT INTO agent_oom_kills(space_id,kills,killed_at,execution_id) VALUES (?1,1,?2,?3)
         ON CONFLICT(space_id) DO UPDATE SET
             kills=kills+1,killed_at=excluded.killed_at,execution_id=excluded.execution_id
         WHERE execution_id IS NOT excluded.execution_id",
        params![id, at, execution],
    )?;
    Ok(counted > 0)
}

/// The OOM kills of the agent of the space `id` since it last finished a
/// turn, if any.
pub fn get(db: &Connection, id: &SpaceId) -> Result<Option<OomKills>> {
    Ok(db
        .query_row(
            "SELECT kills,killed_at FROM agent_oom_kills WHERE space_id=?1",
            [id],
            |row| {
                Ok(OomKills {
                    kills: row.get(0)?,
                    killed_at: row.get(1)?,
                })
            },
        )
        .optional()?)
}

/// Forget the kills of the agent of the space `id` once it finished a turn,
/// going from `previous`, working, to `state`, idle, without the turn
/// failing.
pub fn follow_state(
    db: &Connection,
    id: &SpaceId,
    previous: Option<AgentState>,
    state: AgentState,
    failed: bool,
) -> Result<()> {
    if previous == Some(AgentState::Working) && state == AgentState::Idle && !failed {
        db.execute("DELETE FROM agent_oom_kills WHERE space_id=?1", [id])?;
    }
    Ok(())
}

impl crate::Agents {
    /// Count the OOM kill that ended `execution`, the agent of the space
    /// `id`, now, and withdraw the healthy readings taken before it; a
    /// failure to count is logged, as it must not fail the agent's stop.
    pub async fn count_oom_kill(&self, id: &SpaceId, execution: &ExecutionId) {
        self.overload_recovery.reset();
        let (space, execution) = (id.to_owned(), execution.to_owned());
        let counted = (self.store)
            .write(move |tx| record(tx, &space, &execution, UnixTime::now()))
            .await;
        if let Err(error) = counted {
            crate::log!("OOM kill of the agent of {id} not counted: {error:#}");
        }
    }

    /// The OOM kills of the agent of the space `id` the store counted since
    /// it last finished a turn, if any.
    pub async fn counted_oom_kills(&self, id: &SpaceId) -> Result<Option<OomKills>> {
        let space = id.to_owned();
        self.store.read(move |db| get(db, &space)).await
    }

    /// The OOM kills the restore of `execution` in the space `id` counts:
    /// those since the agent last finished a turn, the one its recovery
    /// record names among them.
    pub async fn oom_kills(
        &self,
        id: &SpaceId,
        execution: &ExecutionId,
    ) -> Result<Option<OomKills>> {
        self.count_recorded_oom_kill(id, execution).await?;
        self.counted_oom_kills(id).await
    }

    /// Count the OOM kills the recovery records at `restored_from` name, which
    /// an agent starting in the space `id` restores from and its wrapper
    /// consumes once the start is acknowledged; a failure is logged, as it
    /// must not fail the start.
    pub async fn count_restored_oom_kills(&self, id: &SpaceId, restored_from: &[PathBuf]) {
        for execution in restored_from
            .iter()
            .filter_map(|path| recovery::record_execution(path))
        {
            if let Err(error) = self.count_recorded_oom_kill(id, &execution).await {
                crate::log!("OOM kill of execution {execution} not counted: {error:#}");
            }
        }
    }

    /// Count the OOM kill the recovery record of `execution` in the space
    /// `id` names, as a reported kill counts, unless it was counted. A
    /// restore consumes that record, and a later kill before a finished turn
    /// must still back off.
    pub async fn count_recorded_oom_kill(
        &self,
        id: &SpaceId,
        execution: &ExecutionId,
    ) -> Result<()> {
        let path = recovery::record_path(&self.paths, id, execution);
        let saved = recovery::Record::load(&path).ok().flatten();
        if !saved.is_some_and(|saved| saved.stop_reason.as_deref() == Some(OOM_REASON)) {
            return Ok(());
        }
        let (space, ended) = (id.to_owned(), execution.to_owned());
        let new = (self.store)
            .write(move |tx| record(tx, &space, &ended, UnixTime::now()))
            .await?;
        if new {
            self.overload_recovery.reset();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
