//! The sleep an agent that waits for something takes: Shoal does not wake
//! it for having stayed idle, and puts it to sleep once that costs nothing,
//! until its next turn starts, or after every turn until `shoal sleep
//! --undo`, which also wakes an agent asleep for any reason.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    client::{sleep::Slept, space_ids::SpaceId, space_names::SpaceSelector},
    daemon::workspace::Manager,
    state::{AgentState, Sleep},
};

/// How long the agent of the space `id` sleeps.
pub(crate) fn current(db: &Connection, id: &SpaceId) -> Result<Option<Sleep>> {
    Ok(db
        .query_row(
            "SELECT sleep FROM space_sleep WHERE space_id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()?)
}

/// Record that the agent of the space `id` sleeps as `sleep` says, or
/// wakes with none.
fn set(db: &Connection, id: &SpaceId, sleep: Option<Sleep>) -> Result<()> {
    match sleep {
        Some(sleep) => db.execute(
            "INSERT INTO space_sleep(space_id,sleep) VALUES (?1,?2)
             ON CONFLICT(space_id) DO UPDATE SET sleep=excluded.sleep",
            params![id, sleep],
        )?,
        None => db.execute("DELETE FROM space_sleep WHERE space_id=?1", [id])?,
    };
    Ok(())
}

/// End the sleep that lasts until the next turn of the agent of the space
/// `id`, in the transaction that records its state going from `previous`
/// to `state`: a turn starts when an agent that was idle, or had reported
/// nothing, works.
pub(crate) fn follow_state(
    db: &Connection,
    id: &SpaceId,
    previous: Option<AgentState>,
    state: AgentState,
) -> Result<()> {
    if state == AgentState::Working && previous.is_none_or(|previous| previous == AgentState::Idle)
    {
        db.execute(
            "DELETE FROM space_sleep WHERE space_id=?1 AND sleep=?2",
            params![id, Sleep::Turn],
        )?;
    }
    Ok(())
}

impl Manager {
    /// Let the agent of the space `selector` names sleep as `sleep` says,
    /// or with none end its sleep and wake it: a restore Shoal queued on its
    /// own for it then waits only for its turn in the agent queue.
    pub async fn sleep(&self, selector: &SpaceSelector, sleep: Option<Sleep>) -> Result<Slept> {
        let space = self.space(selector).await?;
        let idle_wake_minutes = self.idle_wake_minutes(&space).await?;
        let id = space.id().to_owned();
        let sleep = (self.store)
            .write(move |tx| {
                set(tx, &id, sleep)?;
                if sleep.is_none() {
                    crate::daemon::workspace::agent_queue::asleep::wake(tx, &id)?;
                }
                current(tx, &id)
            })
            .await?;
        self.agent_queue.wake();
        Ok(Slept {
            space: space.name().to_owned(),
            sleep,
            idle_wake_minutes,
        })
    }
}
