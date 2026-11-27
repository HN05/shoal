//! The OOM kills that ended a space's agent since it last finished a turn,
//! and how long they hold back its next restore.
//! `docs/design/agent-lifecycle.md` says why the restore backs off.
use std::time::Duration;

use anyhow::Result;
use rusqlite::Connection;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;

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
    /// and keep in its recovery record how many kills ended it in a row and
    /// when its restore may start. A failure leaves the restore unheld. The
    /// kill withdraws healthy readings taken before it, as a stop does.
    pub async fn record_oom_kill(&self, space_id: &SpaceId, execution: &ExecutionId) {
        self.overload_recovery.reset();
        let id = space_id.clone();
        let saved = match self.store.write(move |tx| count_kill(tx, &id)).await {
            Ok(kills) => {
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

/// Count a further OOM kill of the agent of the space `id`, returning how
/// many ended it in a row.
fn count_kill(db: &Connection, id: &SpaceId) -> Result<u32> {
    Ok(db.query_row(
        "INSERT INTO space_oom_kills(space_id,kills) VALUES (?1,1)
         ON CONFLICT(space_id) DO UPDATE SET kills=kills+1
         RETURNING kills",
        [id],
        |row| row.get(0),
    )?)
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
