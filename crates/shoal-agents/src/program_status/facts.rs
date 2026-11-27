//! What the space's status follows, read from its records together.
use anyhow::Result;
use rusqlite::Connection;
use shoal_ids::{execution_ids::ExecutionId, space_ids::SpaceId};

use crate::{
    daemon::{Records, SpaceStatus, Steps, Waiting},
    queue,
    state::{AgentState, TurnFailure},
    turn_retry, turn_state,
};

pub struct Facts {
    /// The space's name.
    pub name: String,
    /// The newest of its threads whose turn is the user's.
    pub thread: Option<Waiting>,
    /// The oldest of its approval requests the user decides.
    pub approval: Option<Waiting>,
    /// Why the agent's turn failed, when Shoal does not retry it.
    pub turn_failure: Option<TurnFailure>,
    /// Its recorded completion, with its summary.
    pub done: Option<Option<String>>,
    /// Its first ready mark, with the URL of the item it marks.
    pub ready: Option<Option<String>>,
    /// Why Shoal stopped its agent, while it brings the agent back.
    pub asleep: Option<String>,
    /// The turn state the agent's hooks last reported.
    pub turn: Option<AgentState>,
    pub progress: Steps,
}

/// The facts of the space `id`, whose agent runs as `execution`, with
/// `pushing` telling which operations still push what the user approved;
/// none once the space is gone.
pub fn read(
    db: &Connection,
    records: &dyn Records,
    id: &SpaceId,
    execution: &ExecutionId,
    pushing: &dyn Fn(&str) -> bool,
) -> Result<Option<Facts>> {
    let Some(space) = records.space_status(db, id, pushing)? else {
        return Ok(None);
    };
    let SpaceStatus {
        name,
        thread,
        approval,
        done,
        ready,
        progress,
    } = space;
    Ok(Some(Facts {
        name,
        thread,
        approval,
        turn_failure: turn_retry::unretried::failure(db, execution)?,
        done,
        ready,
        // Its reason is the same whether readings are on or off.
        asleep: queue::asleep::current(db, id, true)?.map(|asleep| asleep.reason),
        turn: turn_state::current(db, id)?.map(|status| status.state),
        progress,
    }))
}
