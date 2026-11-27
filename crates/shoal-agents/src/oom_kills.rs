//! The OOM kills that ended a space's agent since it last finished a turn,
//! and how long they hold back its next restore.
//! `docs/design/agent-lifecycle.md` says why the restore backs off.
use std::time::Duration;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension as _};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::stop_causes::StopCause;

use crate::Agents;
use crate::execution::recovery;
use crate::state::AgentState;
use crate::time::UnixTime;

/// The delay after the second kill in a row, which doubles with each further
/// kill up to [`DOUBLED_DELAYS`] times.
const FIRST_DELAY: Duration = Duration::from_secs(5 * 60);

/// How often the delay doubles before it reaches [`LONGEST_DELAY`].
const DOUBLED_DELAYS: u32 = 3;

const LONGEST_DELAY: Duration = Duration::from_secs(2 * 60 * 60);

impl Agents {
    /// Count the OOM kill that ended the agent of `execution` in `space_id`,
    /// once whichever reports it, and keep in its recovery record how many
    /// kills ended it in a row and when its restore may start. A failure
    /// leaves the restore unheld. The kill withdraws healthy readings taken
    /// before it, as a stop does.
    pub async fn record_oom_kill(&self, space_id: &SpaceId, execution: &ExecutionId) {
        let (id, killed) = (space_id.clone(), execution.clone());
        let saved = match self
            .store
            .write(move |tx| count_kill(tx, &id, &killed))
            .await
        {
            Ok(Counted { kills, new }) => {
                if new {
                    self.overload_recovery.reset();
                }
                let after = delay(kills).map(|delay| UnixTime::now() + delay);
                recovery::save_oom_kills(&self.paths, space_id, execution, kills, after)
            }
            Err(error) => Err(error),
        };
        if let Err(error) = saved {
            crate::log!("OOM kill of execution {execution} not counted: {error:#}");
        }
    }
}

impl Agents {
    /// Count the OOM kill its wrapper marked in the record of `agent`, which
    /// `execution` ran in `space_id`, as its pause report does, and log the
    /// stop the report would have logged. A kill the report counted already
    /// counts and logs once.
    pub async fn recover_oom_kill(
        &self,
        space_id: &SpaceId,
        execution: &ExecutionId,
        agent: crate::agent::Agent,
    ) {
        let stopped = shoal_daemon_base::events::AgentEvent {
            mode: None,
            name: agent,
            execution_id: Some(execution.clone()),
            exit_code: None,
            oom_kills: 0,
            cause: Some(StopCause::OomKill),
            source: None,
        };
        let kind = shoal_daemon_base::events::EventKind::AgentStopped;
        shoal_daemon_base::events::record_agent(&self.store, space_id, kind, stopped).await;
        self.record_oom_kill(space_id, execution).await;
    }
}

/// The OOM kills that ended a space's agent in a row.
struct Counted {
    kills: u32,
    /// Whether this count added the kill, rather than found it counted.
    new: bool,
}

/// Count the OOM kill that ended the agent `execution` ran in the space
/// `id`, unless it is counted already, as the latest.
fn count_kill(db: &Connection, id: &SpaceId, execution: &ExecutionId) -> Result<Counted> {
    let counted: Option<String> = db
        .query_row(
            "SELECT execution_id FROM space_oom_kills WHERE space_id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()?;
    let new = counted.as_deref() != Some(execution.as_str());
    let kills = db.query_row(
        "INSERT INTO space_oom_kills(space_id,kills,execution_id) VALUES (?1,1,?2)
         ON CONFLICT(space_id) DO UPDATE SET kills=kills+?3, execution_id=excluded.execution_id
         RETURNING kills",
        rusqlite::params![id, execution, new],
        |row| row.get(0),
    )?;
    Ok(Counted { kills, new })
}

/// Forget the OOM kills of the space `id` once its agent finishes a turn,
/// going from `previous`, working, to `state`, idle.
pub(crate) fn follow_state(
    db: &Connection,
    id: &SpaceId,
    previous: Option<AgentState>,
    state: AgentState,
) -> Result<()> {
    if previous == Some(AgentState::Working) && state == AgentState::Idle {
        db.execute("DELETE FROM space_oom_kills WHERE space_id=?1", [id])?;
    }
    Ok(())
}

/// How long the restore after `kills` OOM kills in a row waits, beyond
/// memory recovering: nothing after the first, so that one kill costs no
/// time, then a delay that doubles before it settles at its longest.
fn delay(kills: u32) -> Option<Duration> {
    let doublings = kills.checked_sub(2)?;
    Some(match doublings {
        doublings if doublings <= DOUBLED_DELAYS => FIRST_DELAY * (1 << doublings),
        _ => LONGEST_DELAY,
    })
}

#[cfg(test)]
mod tests;
