//! The user's interactions with each space's agent, which keep it awake
//! while idle: whether the user is engaged with it, having interacted since
//! its turn before the last ended, and how long they take to come back to it
//! after a turn, which sets how long it waits for them before it sleeps.
use shoal_ids::{execution_ids::ExecutionId, space_ids::SpaceId};
use shoal_protocol::space_names::SpaceSelector;
use std::{collections::HashMap, time::Duration};

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

use super::manager::Manager;
use crate::daemon::{log, scope::Caller};
use crate::{state::AgentState, time::UnixTime};

/// How long an idle agent waits before it sleeps, at the least, which
/// lets its hooks settle and a follow-up turn start.
pub(crate) const FLOOR: Duration = Duration::from_secs(10);

/// How many times the user's reply time an agent they are engaged with
/// waits for them.
const MARGIN: u32 = 2;

/// How many of a space's latest reply times make its own average.
const SPACE_REPLIES: i64 = 10;

/// How many of the latest reply times over every space make the user's
/// average, which a space without its own takes.
const USER_REPLIES: i64 = 50;

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

    /// When an agent idle since `idle_since` stops waiting for the user:
    /// after [`FLOOR`] for one they are not engaged with; for one they are,
    /// after [`MARGIN`] times their `reply` time, between the floor and
    /// `max_wait`, or `max_wait` without one, counted from their last
    /// interaction where that came later.
    pub(crate) fn wait_ends(
        &self,
        idle_since: UnixTime,
        reply: Option<Duration>,
        max_wait: Duration,
    ) -> UnixTime {
        if !self.engaged() {
            return idle_since + FLOOR;
        }
        let from = (self.interacted_at).map_or(idle_since, |at| at.max(idle_since));
        let max_wait = max_wait.max(FLOOR);
        from + reply.map_or(max_wait, |reply| (reply * MARGIN).clamp(FLOOR, max_wait))
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

/// How long the user takes to come back to the agent of the space `id`
/// after a turn: the average of the space's latest reply times, else of
/// the user's over every space, each counted at most `cap`; none without
/// any.
fn reply_time(db: &Connection, id: &SpaceId, cap: Duration) -> Result<Option<Duration>> {
    let cap = i64::try_from(cap.as_secs())?;
    let average = |space: Option<&SpaceId>, count: i64| -> Result<Option<f64>> {
        Ok(db.query_row(
            "SELECT AVG(min(seconds,?2)) FROM (SELECT seconds FROM reply_times
                 WHERE ?1 IS NULL OR space_id=?1 ORDER BY id DESC LIMIT ?3)",
            params![space, cap, count],
            |row| row.get(0),
        )?)
    };
    let average = match average(Some(id), SPACE_REPLIES)? {
        Some(average) => Some(average),
        None => average(None, USER_REPLIES)?,
    };
    Ok(average.map(Duration::from_secs_f64))
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

    /// When the idle agent of the space `id`, idle since `idle_since`, goes
    /// to sleep under the space's `[sleep]` settings; none where they keep
    /// idle agents running.
    pub(crate) async fn sleeps_at(
        &self,
        id: &SpaceId,
        idle_since: UnixTime,
    ) -> Result<Option<UnixTime>> {
        let space = self.space_by_id(id).await?;
        let sleep = self.space_settings(&space).await?.sleep.clone();
        if !sleep.enabled {
            return Ok(None);
        }
        let max_wait = Duration::from_secs(sleep.max_wait_minutes.saturating_mul(60));
        let id = id.to_owned();
        let (engagement, reply) = (self.store)
            .read(move |db| Ok((get(db, &id)?, reply_time(db, &id, max_wait)?)))
            .await?;
        Ok(Some(engagement.wait_ends(idle_since, reply, max_wait)))
    }

    /// Whether the user is engaged with the agent of the space `id`.
    pub(crate) async fn engaged(&self, id: &SpaceId) -> Result<bool> {
        let id = id.to_owned();
        Ok(self.store.read(move |db| get(db, &id)).await?.engaged())
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

    /// Record that the user interacted with the agent of each space their
    /// message `reached`, when `caller` is the user.
    pub(crate) async fn record_users_message(
        &self,
        caller: Option<&Caller>,
        reached: &shoal_protocol::agent_messages::Reached,
    ) {
        let spaces = [&reached.now, &reached.on_start].into_iter();
        let recipients: Vec<SpaceSelector> = spaces
            .flat_map(|spaces| spaces.workspaces.iter().chain(&spaces.chats))
            .map(|recipient| SpaceSelector::id(&recipient.id))
            .collect();
        for recipient in &recipients {
            self.record_user_interaction(caller, recipient).await;
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
