//! Each trigger's runtime state in the store: whether it is paused, the
//! times its source counts from, its newest runs and the keys of the runs
//! it started.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::launches::Launch;
use shoal_protocol::triggers::{RunOutcome, TriggerRun};

use crate::time::UnixTime;

/// How many runs a trigger's history keeps.
const HISTORY: i64 = 50;

/// A trigger's runtime state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct State {
    pub paused: bool,
    /// When Shoal first read the trigger.
    pub created_at: UnixTime,
    /// Its source counts the times it would fire after this.
    pub counted_from: UnixTime,
    /// The last firing that reached a run.
    pub last_fired_at: Option<UnixTime>,
}

impl State {
    /// The last firing, or before the first, when Shoal first read the
    /// trigger, which a firing's input names as the time before it.
    pub fn previous(&self) -> UnixTime {
        self.last_fired_at.unwrap_or(self.created_at)
    }
}

/// What a queued run launches once the agent of the trigger's last space
/// stops: its launch, and the key it keeps once started.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct Waiting {
    pub launch: Launch,
    pub key: Option<String>,
}

/// A queued run of a trigger.
pub(super) struct Queued {
    pub id: i64,
    /// The space whose agent it waits for.
    pub waits_for: SpaceId,
    pub waiting: Waiting,
}

/// The state of the trigger `name` of `scope`, recorded as new and active
/// at `now` the first time it is read.
pub(super) fn state(db: &Connection, scope: &str, name: &str, now: UnixTime) -> Result<State> {
    db.execute(
        "INSERT OR IGNORE INTO trigger_states(scope,name,created_at,counted_from)
            VALUES (?1,?2,?3,?3)",
        params![scope, name, now],
    )?;
    Ok(db.query_row(
        "SELECT paused,created_at,counted_from,last_fired_at FROM trigger_states
            WHERE scope=?1 AND name=?2",
        params![scope, name],
        |row| {
            Ok(State {
                paused: row.get(0)?,
                created_at: row.get(1)?,
                counted_from: row.get(2)?,
                last_fired_at: row.get(3)?,
            })
        },
    )?)
}

/// Have the trigger's source count from `to`.
pub(super) fn count_from(db: &Connection, scope: &str, name: &str, to: UnixTime) -> Result<()> {
    db.execute(
        "UPDATE trigger_states SET counted_from=?3 WHERE scope=?1 AND name=?2",
        params![scope, name, to],
    )?;
    Ok(())
}

/// Record that the trigger fired at `at`.
pub(super) fn fired(db: &Connection, scope: &str, name: &str, at: UnixTime) -> Result<()> {
    db.execute(
        "UPDATE trigger_states SET last_fired_at=?3 WHERE scope=?1 AND name=?2",
        params![scope, name, at],
    )?;
    Ok(())
}

/// Add `run` to the trigger's history, with what a queued run waits to
/// launch, and drop the oldest runs beyond the history.
pub(super) fn record(
    db: &Connection,
    scope: &str,
    name: &str,
    run: &TriggerRun,
    waiting: Option<&Waiting>,
) -> Result<()> {
    let waiting = waiting.map(serde_json::to_string).transpose()?;
    db.execute(
        "INSERT INTO trigger_runs(scope,name,at,origin,outcome,reason,space_id,launch)
            VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            scope,
            name,
            run.at,
            run.origin,
            run.outcome,
            run.reason,
            run.space_id,
            waiting,
        ],
    )?;
    db.execute(
        "DELETE FROM trigger_runs WHERE scope=?1 AND name=?2 AND outcome<>?4 AND id NOT IN (
            SELECT id FROM trigger_runs WHERE scope=?1 AND name=?2 ORDER BY id DESC LIMIT ?3)",
        params![scope, name, HISTORY, RunOutcome::Queued],
    )?;
    Ok(())
}

/// The space the trigger's newest started run started.
pub(super) fn last_space(db: &Connection, scope: &str, name: &str) -> Result<Option<SpaceId>> {
    Ok(db
        .query_row(
            "SELECT space_id FROM trigger_runs
                WHERE scope=?1 AND name=?2 AND outcome=?3 AND space_id IS NOT NULL
                ORDER BY id DESC LIMIT 1",
            params![scope, name, RunOutcome::Started],
            |row| row.get(0),
        )
        .optional()?)
}

/// The trigger's run that waits to launch, if any.
pub(super) fn queued(db: &Connection, scope: &str, name: &str) -> Result<Option<Queued>> {
    let row = db
        .query_row(
            "SELECT id,space_id,launch FROM trigger_runs
                WHERE scope=?1 AND name=?2 AND outcome=?3 ORDER BY id LIMIT 1",
            params![scope, name, RunOutcome::Queued],
            |row| Ok((row.get(0)?, row.get(1)?, row.get::<_, String>(2)?)),
        )
        .optional()?;
    let Some((id, waits_for, waiting)) = row else {
        return Ok(None);
    };
    Ok(Some(Queued {
        id,
        waits_for,
        waiting: serde_json::from_str(&waiting)?,
    }))
}

/// Record how the queued run `id` ended: started in `space`, or failed for
/// `reason`.
pub(super) fn settle(
    db: &Connection,
    id: i64,
    outcome: RunOutcome,
    reason: Option<&str>,
    space: Option<&SpaceId>,
) -> Result<()> {
    db.execute(
        "UPDATE trigger_runs SET outcome=?2,reason=?3,space_id=?4,launch=NULL WHERE id=?1",
        params![id, outcome, reason, space],
    )?;
    Ok(())
}

/// Whether a run the trigger started within its window had `key`; keys
/// whose window ended are forgotten.
pub(super) fn key_is_live(
    db: &Connection,
    scope: &str,
    name: &str,
    key: &str,
    now: UnixTime,
) -> Result<bool> {
    db.execute(
        "DELETE FROM trigger_keys WHERE expires_at<=?1",
        params![now],
    )?;
    Ok(db
        .query_row(
            "SELECT 1 FROM trigger_keys WHERE scope=?1 AND name=?2 AND key=?3",
            params![scope, name, key],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Keep `key` of a run the trigger started until `expires_at`.
pub(super) fn keep_key(
    db: &Connection,
    scope: &str,
    name: &str,
    key: &str,
    expires_at: UnixTime,
) -> Result<()> {
    db.execute(
        "INSERT INTO trigger_keys(scope,name,key,expires_at) VALUES (?1,?2,?3,?4)
            ON CONFLICT(scope,name,key) DO UPDATE SET expires_at=excluded.expires_at",
        params![scope, name, key, expires_at],
    )?;
    Ok(())
}
