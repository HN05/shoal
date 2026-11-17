//! The user's interactions with each space's agent, which keep it awake
//! while idle: whether the user is engaged with it, having interacted since
//! its turn before the last ended, and how long they take to come back to it
//! after a turn.
use shoal_ids::{execution_ids::ExecutionId, space_ids::SpaceId};
use shoal_protocol::space_names::SpaceSelector;
use std::{collections::HashMap, time::Duration};

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

use super::Manager;
use crate::daemon::{log, scope::Caller};
use crate::{state::AgentState, time::UnixTime};

/// How many reply times the store keeps, the latest.
const KEPT_REPLIES: i64 = 500;

/// What the store knows of the user's interactions with a space's agent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Engagement {
    /// When the user last interacted with it.
    pub interacted_at: Option<UnixTime>,
    /// When its last turn ended.
    pub turn_ended_at: Option<UnixTime>,
    /// When the turn before that ended.
    pub previous_turn_ended_at: Option<UnixTime>,
}

impl Engagement {
    /// Whether the user is engaged with the agent: they interacted with it
    /// since its turn before the last ended.
    pub(crate) fn engaged(&self) -> bool {
        self.interacted_at.is_some_and(|interacted| {
            (self.previous_turn_ended_at).is_none_or(|ended| interacted > ended)
        })
    }

    /// The reply time an interaction at `at` gives: how long after the last
    /// turn's end it came, when it is the first since that turn ended and
    /// the user was engaged with the turn.
    fn reply(&self, at: UnixTime) -> Option<Duration> {
        let ended = self.turn_ended_at.filter(|ended| at >= *ended)?;
        let first = (self.interacted_at).is_none_or(|interacted| interacted < ended);
        (first && self.engaged()).then(|| at.duration_since(ended))
    }
}

/// The engagement recorded for the space `id`.
pub(crate) fn get(db: &Connection, id: &SpaceId) -> Result<Engagement> {
    let engagement = db
        .query_row(
            "SELECT interacted_at,turn_ended_at,previous_turn_ended_at
             FROM space_engagement WHERE space_id=?1",
            [id],
            |row| {
                Ok(Engagement {
                    interacted_at: row.get(0)?,
                    turn_ended_at: row.get(1)?,
                    previous_turn_ended_at: row.get(2)?,
                })
            },
        )
        .optional()?;
    Ok(engagement.unwrap_or_default())
}

/// Record the turn that a report of `state` after `previous` ends at `at`
/// for the agent of the space `id`: one that leaves working or waiting for
/// idle. A session that starts idle ends none.
pub(crate) fn follow_state(
    db: &Connection,
    id: &SpaceId,
    previous: Option<AgentState>,
    state: AgentState,
    at: UnixTime,
) -> Result<()> {
    let ends_turn = state == AgentState::Idle
        && matches!(previous, Some(AgentState::Working | AgentState::Waiting));
    if ends_turn {
        db.execute(
            "INSERT INTO space_engagement(space_id,turn_ended_at) VALUES (?1,?2)
             ON CONFLICT(space_id) DO UPDATE SET
                 previous_turn_ended_at=turn_ended_at,turn_ended_at=excluded.turn_ended_at",
            params![id, at],
        )?;
    }
    Ok(())
}

/// Record that the user interacted with the agent of the space `id` at
/// `at`, with the reply time it gives.
fn interact(db: &Connection, id: &SpaceId, at: UnixTime) -> Result<()> {
    if let Some(reply) = get(db, id)?.reply(at) {
        db.execute(
            "INSERT INTO reply_times(space_id,seconds) VALUES (?1,?2)",
            params![id, i64::try_from(reply.as_secs())?],
        )?;
        db.execute(
            "DELETE FROM reply_times WHERE id <=
                 (SELECT id FROM reply_times ORDER BY id DESC LIMIT 1 OFFSET ?1)",
            [KEPT_REPLIES],
        )?;
    }
    db.execute(
        "INSERT INTO space_engagement(space_id,interacted_at) VALUES (?1,?2)
         ON CONFLICT(space_id) DO UPDATE SET
             interacted_at=max(COALESCE(interacted_at,excluded.interacted_at),excluded.interacted_at)",
        params![id, at],
    )?;
    Ok(())
}

/// When the user last interacted with the agent of each space.
fn interactions(db: &Connection) -> Result<HashMap<SpaceId, UnixTime>> {
    Ok(db
        .prepare(
            "SELECT space_id,interacted_at FROM space_engagement WHERE interacted_at IS NOT NULL",
        )?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

impl Manager {
    /// Record that the user interacted with the agent of the space `id` at
    /// `at`.
    pub(crate) async fn record_interaction(&self, id: &SpaceId, at: UnixTime) -> Result<()> {
        let id = id.to_owned();
        self.store.write(move |tx| interact(tx, &id, at)).await
    }

    /// When the user last interacted with the agent of each space.
    pub(crate) async fn interactions(&self) -> Result<HashMap<SpaceId, UnixTime>> {
        self.store.read(|db| interactions(db)).await
    }

    /// Record that the user interacted with the agent of the space
    /// `selector` names, when `caller` is the user: a scoped caller, such
    /// as a coordinator, is not.
    pub(crate) async fn record_user_interaction(
        &self,
        caller: Option<&Caller>,
        selector: &SpaceSelector,
    ) {
        if caller.is_none() {
            self.note_interaction(selector).await;
        }
    }

    /// Record that the user interacted with the agent of the space
    /// `selector` names now. A failure only logs, since the request it comes
    /// with goes on.
    pub(crate) async fn note_interaction(&self, selector: &SpaceSelector) {
        let recorded = async {
            let id = self.space(selector).await?.id;
            self.record_interaction(&id, UnixTime::now()).await
        };
        if let Err(error) = recorded.await {
            log!("interaction with {selector} not recorded: {error:#}");
        }
    }

    /// Record that the user interacted with the agent `execution` runs, as
    /// by a key on its sleep screen, logging a failure.
    pub(crate) async fn record_agent_interaction(&self, execution: &ExecutionId) {
        let id = execution.to_owned();
        let recorded = self.store.write(move |tx| {
            let Some(execution) = crate::daemon::store::find_execution(tx, &id)? else {
                return Ok(());
            };
            interact(tx, &execution.space_id, UnixTime::now())
        });
        if let Err(error) = recorded.await {
            log!("interaction with execution {execution} not recorded: {error:#}");
        }
    }
}

#[cfg(test)]
mod tests;
