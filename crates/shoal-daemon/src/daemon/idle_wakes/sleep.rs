//! The sleep an agent that waits for something takes, so Shoal does not
//! wake it for having stayed idle: until its next turn starts, or until
//! `shoal sleep --undo`.
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
    /// or end its sleep with none. A sleep does nothing in a space whose
    /// mode never wakes its agent.
    pub async fn sleep(&self, selector: &SpaceSelector, sleep: Option<Sleep>) -> Result<Slept> {
        let space = self.space(selector).await?;
        let idle_wake_minutes = self.idle_wake_minutes(&space).await?;
        let id = space.id().to_owned();
        let changes = sleep.is_none() || idle_wake_minutes.is_some();
        let sleep = (self.store)
            .write(move |db| {
                if changes {
                    set(db, &id, sleep)?;
                }
                current(db, &id)
            })
            .await?;
        Ok(Slept {
            space: space.name().to_owned(),
            sleep,
            idle_wake_minutes,
        })
    }
}
