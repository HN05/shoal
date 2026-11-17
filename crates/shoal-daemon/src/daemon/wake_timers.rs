//! The timer `shoal sleep --for` or `--until` sets for a space: when it is
//! due, Shoal sends the space's agent its message, which reaches an awake
//! agent through its hooks and starts an asleep one's restore, as any
//! message does. A space has one timer, which a new one replaces; it lasts
//! until it fires, `shoal sleep --undo` cancels it, or the space is done
//! or removed, through agent stops, restores and daemon restarts.
use std::{sync::Arc, time::Duration};

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::{agent_messages, log, workspace::Manager};
use crate::time::UnixTime;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::sleep::WakeTimer;

/// The longest the loop waits before reading the clock again, so a timer
/// fires on time after the machine slept, which pauses the waits' clock.
const MAX_WAIT: Duration = Duration::from_secs(60);

/// How long a timer whose message could not be queued waits to fire again.
const RETRY_DELAY: Duration = Duration::from_secs(60);

/// Give the space `id` `timer`, replacing the one it had.
pub(crate) fn set(db: &Connection, id: &SpaceId, timer: &WakeTimer) -> Result<()> {
    db.execute(
        "INSERT INTO space_wake_timers(space_id,due_at,message) VALUES (?1,?2,?3)
         ON CONFLICT(space_id) DO UPDATE SET due_at=excluded.due_at,message=excluded.message",
        params![id, timer.at, timer.message],
    )?;
    Ok(())
}

/// Cancel the timer of the space `id`, if it has one.
pub(crate) fn cancel(db: &Connection, id: &SpaceId) -> Result<()> {
    db.execute("DELETE FROM space_wake_timers WHERE space_id=?1", [id])?;
    Ok(())
}

/// When the timer of the space `id` fires, if it has one.
pub(crate) fn due_at(db: &Connection, id: &SpaceId) -> Result<Option<UnixTime>> {
    Ok(db
        .query_row(
            "SELECT due_at FROM space_wake_timers WHERE space_id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()?)
}

/// The spaces whose timer is due at `now`.
fn due_spaces(db: &Connection, now: UnixTime) -> Result<Vec<SpaceId>> {
    Ok(db
        .prepare("SELECT space_id FROM space_wake_timers WHERE due_at<=?1")?
        .query_map([now], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// When the next timer fires.
fn next_due(db: &Connection) -> Result<Option<UnixTime>> {
    Ok(
        db.query_row("SELECT MIN(due_at) FROM space_wake_timers", [], |row| {
            row.get(0)
        })?,
    )
}

/// Fire the timer of the space `id` when it is still due at `now`: queue
/// its message and delete it together, so a timer set meanwhile is kept.
/// Whether a message was queued.
fn fire(tx: &Transaction<'_>, id: &SpaceId, now: UnixTime) -> Result<bool> {
    let message: Option<Option<String>> = tx
        .query_row(
            "DELETE FROM space_wake_timers WHERE space_id=?1 AND due_at<=?2 RETURNING message",
            params![id, now],
            |row| row.get(0),
        )
        .optional()?;
    let Some(message) = message else {
        return Ok(false);
    };
    let queued = agent_messages::queue_shoal_message(tx, id, &delivery(message.as_deref()))?;
    Ok(queued.is_some())
}

/// Hold the timer of the space `id` back until `at`, after a firing failed.
fn postpone(db: &Connection, id: &SpaceId, at: UnixTime) -> Result<()> {
    db.execute(
        "UPDATE space_wake_timers SET due_at=?2 WHERE space_id=?1",
        params![id, at],
    )?;
    Ok(())
}

/// What the agent receives when its timer fires with `message`.
fn delivery(message: Option<&str>) -> String {
    match message {
        Some(message) => format!("The timer you set with `shoal sleep` fired: {message}"),
        None => "The timer you set with `shoal sleep` fired.".to_owned(),
    }
}

impl Manager {
    /// Whether the space `id` has a timer.
    pub(crate) async fn wake_timer_set(&self, id: &SpaceId) -> Result<bool> {
        let id = id.to_owned();
        let due_at = self.store.read(move |tx| due_at(tx, &id)).await?;
        Ok(due_at.is_some())
    }

    /// Fire the timers due at `now`, each on its own; when the next one is
    /// due.
    pub(crate) async fn fire_wake_timers(&self, now: UnixTime) -> Result<Option<UnixTime>> {
        let mut queued = false;
        for id in self.store.read(move |db| due_spaces(db, now)).await? {
            queued |= self.fire_wake_timer(id, now).await?;
        }
        if queued {
            self.signals.message_queued();
        }
        self.store.read(|tx| next_due(tx)).await
    }

    /// Fire the timer of the space `id` due at `now`, or hold it back
    /// [`RETRY_DELAY`] when its message cannot be queued, such as to a full
    /// message queue, so it holds up no other timer. Whether a message was
    /// queued.
    async fn fire_wake_timer(&self, id: SpaceId, now: UnixTime) -> Result<bool> {
        let space = id.clone();
        match self.store.write(move |tx| fire(tx, &space, now)).await {
            Ok(queued) => Ok(queued),
            Err(error) => {
                log!("wake timer of space {id}: {error:#}");
                let later = now + RETRY_DELAY;
                (self.store)
                    .write(move |tx| postpone(tx, &id, later))
                    .await?;
                Ok(false)
            }
        }
    }
}

/// Fire each timer when it is due, waking early when one is set.
pub(super) async fn run(manager: Arc<Manager>) {
    loop {
        let next = {
            let _operation = manager.gates.operation().await;
            manager.fire_wake_timers(UnixTime::now()).await
        };
        let wait = match next {
            Ok(Some(due)) => due
                .duration_since(UnixTime::now())
                .clamp(Duration::from_secs(1), MAX_WAIT),
            Ok(None) => MAX_WAIT,
            Err(error) => {
                log!("wake timers: {error:#}");
                MAX_WAIT
            }
        };
        tokio::select! {
            () = manager.signals.wake_timer_set() => {}
            () = tokio::time::sleep(wait) => {}
        }
    }
}

#[cfg(test)]
mod tests;
