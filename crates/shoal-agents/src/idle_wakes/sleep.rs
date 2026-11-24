//! The sleep an agent that waits for something takes: Shoal does not wake
//! it for having stayed idle until its next turn starts, or after every
//! turn until `shoal sleep --undo`, which also wakes an agent asleep for any
//! reason and cancels its timer. Its stop is the one every idle agent takes
//! once the user is away from it.
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::{
    Agents,
    daemon::{Daemon, Records},
    state::{AgentState, Sleep},
    time::UnixTime,
    wake_timers,
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    sleep::{Slept, WakeTimer},
    space_names::SpaceSelector,
};

/// How long the agent of the space `id` sleeps.
pub fn current(db: &Connection, id: &SpaceId) -> Result<Option<Sleep>> {
    Ok(db
        .prepare_cached("SELECT sleep FROM space_sleep WHERE space_id=?1")?
        .query_row([id], |row| row.get(0))
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
pub fn follow_state(
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

/// Refuse a timer without a sleep, since waking the agent cancels its
/// timer, and a timer's message an agent message could not carry.
fn check(sleep: Option<Sleep>, timer: Option<&WakeTimer>) -> Result<()> {
    ensure!(
        sleep.is_some() || timer.is_none(),
        "a timer comes with a sleep; waking the agent cancels its timer"
    );
    if let Some(message) = timer.and_then(|timer| timer.message.as_deref()) {
        crate::validate::agent_message(message)?;
    }
    Ok(())
}

/// Record that the agent of the space `id` sleeps as `sleep` says, with
/// `timer` replacing its timer when one is given, or with no sleep wake it
/// and cancel its timer; how long it then sleeps, and when its timer fires.
fn record(
    tx: &Transaction<'_>,
    records: &dyn Records,
    id: &SpaceId,
    sleep: Option<Sleep>,
    timer: Option<WakeTimer>,
) -> Result<(Option<Sleep>, Option<UnixTime>)> {
    set(tx, id, sleep)?;
    match (sleep, timer) {
        (None, _) => {
            wake_timers::cancel(tx, id)?;
            records.wake_asleep(tx, id)?;
        }
        (Some(_), Some(timer)) => wake_timers::set(tx, id, &timer)?,
        (Some(_), None) => {}
    }
    Ok((current(tx, id)?, wake_timers::due_at(tx, id)?))
}

impl Agents {
    /// Let the agent of the space `selector` names sleep as `sleep` says,
    /// replacing its timer with `timer` when one is given, or with no sleep
    /// end its sleep, cancel its timer and wake it: a restore Shoal queued
    /// on its own for it then waits only for its turn in the agent queue.
    pub async fn sleep(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        sleep: Option<Sleep>,
        timer: Option<WakeTimer>,
    ) -> Result<Slept> {
        check(sleep, timer.as_ref())?;
        let space = daemon.space(selector).await?;
        let idle_wake_minutes = self.idle_wake_minutes(daemon, &space).await?;
        let (id, timer_set) = (space.id.to_owned(), timer.is_some());
        let records = self.records.clone();
        let (sleep, wake_at) = (self.store)
            .write(move |tx| record(tx, &*records, &id, sleep, timer))
            .await?;
        if timer_set {
            self.signals.set_wake_timer();
        }
        self.agent_queue.wake();
        Ok(Slept {
            space: space.name.to_owned(),
            sleep,
            wake_at,
            idle_wake_minutes,
        })
    }
}
