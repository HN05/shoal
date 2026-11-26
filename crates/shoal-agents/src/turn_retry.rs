//! Retrying a turn an API failure ended. `docs/design/agents.md` says when a
//! retry runs. The retry is persisted, so it outlasts a daemon restart and the
//! agent's sleep, and knows no agent: any report of a failed turn schedules
//! one.
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use std::{sync::Arc, time::Duration};

use anyhow::Result;

use rusqlite::{Connection, OptionalExtension, Transaction, params};
pub use shoal_protocol::executions::{FailedTurn, TurnRetry};

use self::unretried::Unretried;
use crate::{
    Agents,
    daemon::{Daemon, Records},
    log, messages,
    model::Space,
    state::{AgentState, QueueKind, TurnFailure},
    time::UnixTime,
};
use shoal_protocol::space_names::SpaceSelector;

pub mod held_wakes;
pub mod unretried;

/// The wait before the first attempt, doubling for each later one.
const FIRST_DELAY_SECS: u64 = 30;
/// The longest wait before an attempt.
const MAX_DELAY_SECS: u64 = 15 * 60;
/// Usage limits may last a week, so keep checking at roughly hourly intervals.
const RATE_LIMIT_MAX_DELAY_SECS: u64 = 60 * 60;
/// How far each wait is spread, as a fraction either way, so agents that
/// failed together do not all retry at once.
const JITTER: f64 = 0.2;
/// How long after a reset the upstream named the agent is asked, so the
/// upstream has surely reset.
pub const RESET_MARGIN: Duration = Duration::from_secs(30);
/// How far off a reset a turn's error states may be: past a weekly usage
/// limit's, so one that is further, such as milliseconds read as seconds,
/// holds nothing back.
const MAX_STATED_RESET: Duration = Duration::from_secs(8 * 24 * 60 * 60);
/// How often the loop looks again without a retry to wait for.
const IDLE_RECHECK: Duration = Duration::from_secs(60 * 60);

pub fn get(db: &Connection, space_id: &SpaceId) -> Result<Option<TurnRetry>> {
    Ok(db
        .query_row(
            "SELECT failure,attempt,due_at,delivered FROM space_turn_retries
             WHERE space_id=?1",
            [space_id],
            |row| {
                Ok(TurnRetry {
                    failure: row.get(0)?,
                    attempt: row.get(1)?,
                    due_at: row.get(2)?,
                    delivered: row.get(3)?,
                })
            },
        )
        .optional()?)
}

/// Follow a reported turn state: a new turn cancels a retry not delivered
/// yet, and the end of the turn a delivered retry started finishes it.
pub(crate) fn follow_state(db: &Connection, space_id: &SpaceId, state: AgentState) -> Result<()> {
    let delivered = match state {
        AgentState::Working => false,
        AgentState::Idle => true,
        AgentState::Waiting => return Ok(()),
    };
    db.execute(
        "DELETE FROM space_turn_retries WHERE space_id=?1 AND delivered=?2",
        params![space_id, delivered],
    )?;
    Ok(())
}

/// The wait before `attempt` for `failure`, spread by `spread` in `[0, 1)`.
fn delay(failure: TurnFailure, attempt: u32, spread: f64) -> Duration {
    let maximum = match failure {
        TurnFailure::RateLimited => RATE_LIMIT_MAX_DELAY_SECS,
        _ => MAX_DELAY_SECS,
    };
    let doublings = attempt.saturating_sub(1).min(16);
    let base = FIRST_DELAY_SECS.saturating_mul(1 << doublings).min(maximum);
    Duration::from_secs((base as f64 * (1.0 - JITTER + 2.0 * JITTER * spread)).round() as u64)
}

/// A random spread in `[0, 1)`.
fn spread() -> f64 {
    (uuid::Uuid::new_v4().as_u128() % 1_000_000) as f64 / 1_000_000.0
}

/// Record the turn `failed` ended in `execution` as its space's retry. It is
/// the next attempt after a delivered one, the first for a new execution or a
/// change between usage limits and transient failures, and the same one when
/// the failure is reported again before it is due. A delivered retry lasts only
/// until the turn it started ends, so a failure while it stands is that turn's,
/// though a restore runs it in a new execution. An attempt waits past the
/// [`known_reset`], read in the same transaction as a report of the proxy's, so
/// neither can slip between.
fn record_failure(
    tx: &Transaction<'_>,
    space_id: &SpaceId,
    execution: &ExecutionId,
    failed: FailedTurn,
    now: UnixTime,
    spread: f64,
) -> Result<()> {
    let reset = known_reset(tx, execution, failed, now)?;
    let previous = previous_attempt(tx, space_id)?;
    let (attempt, due_at) = next_attempt(previous, execution, failed.failure, now, spread);
    let due_at = reset.map_or(due_at, |reset| due_at.max(reset + RESET_MARGIN));
    let retry = Retry {
        failure: failed.failure,
        attempt,
        due_at,
        resets_at: reset,
    };
    save_retry(tx, space_id, execution, &retry)?;
    Ok(())
}

/// When the upstream refuses requests of `execution` until, as far as
/// Shoal knows after the turn `failed` ended at `now`: the later of what
/// its proxy last reported and, for a rate limit, what the turn's error
/// states, unless that is more than [`MAX_STATED_RESET`] away.
fn known_reset(
    db: &Connection,
    execution: &ExecutionId,
    failed: FailedTurn,
    now: UnixTime,
) -> Result<Option<UnixTime>> {
    let stated = (failed.resets_at).filter(|reset| {
        failed.failure == TurnFailure::RateLimited && *reset <= now + MAX_STATED_RESET
    });
    Ok(upstream_reset(db, execution)?.max(stated))
}

/// Keep `reset`, when the upstream of `execution`'s proxy refuses requests
/// until, and hold its pending turn retry until past it, together, so a
/// failure recorded at the same time sees one or the other; `None` forgets
/// it. Whether a pending retry was held.
fn record_reset(
    tx: &Transaction<'_>,
    execution: &ExecutionId,
    reset: Option<UnixTime>,
) -> Result<bool> {
    let Some(reset) = reset else {
        forget_upstream_reset(tx, execution)?;
        return Ok(false);
    };
    tx.execute(
        "INSERT INTO execution_upstream_resets(execution_id,resets_at) VALUES (?1,?2)
         ON CONFLICT(execution_id) DO UPDATE SET resets_at=excluded.resets_at",
        params![execution, reset],
    )?;
    let held = tx.execute(
        "UPDATE space_turn_retries SET due_at=MAX(due_at,?2),
             resets_at=MAX(COALESCE(resets_at,?3),?3)
         WHERE execution_id=?1 AND delivered=0",
        params![execution, reset + RESET_MARGIN, reset],
    )?;
    Ok(held > 0)
}

/// When the upstream of `execution`'s proxy last said it refuses requests
/// until.
pub fn upstream_reset(db: &Connection, execution: &ExecutionId) -> Result<Option<UnixTime>> {
    Ok(db
        .query_row(
            "SELECT resets_at FROM execution_upstream_resets WHERE execution_id=?1",
            [execution],
            |row| row.get(0),
        )
        .optional()?)
}

/// Forget the reset `execution`'s proxy reported.
fn forget_upstream_reset(db: &Connection, execution: &ExecutionId) -> Result<()> {
    db.execute(
        "DELETE FROM execution_upstream_resets WHERE execution_id=?1",
        [execution],
    )?;
    Ok(())
}

/// The persisted attempt whose execution and policy determine its successor.
struct PreviousAttempt {
    execution: ExecutionId,
    failure: TurnFailure,
    attempt: u32,
    due_at: UnixTime,
    delivered: bool,
}

/// Decode the space's most recent attempt, when one is recorded.
fn previous_attempt(db: &Connection, space_id: &SpaceId) -> Result<Option<PreviousAttempt>> {
    Ok(db
        .query_row(
            "SELECT execution_id,failure,attempt,due_at,delivered FROM space_turn_retries
             WHERE space_id=?1",
            [space_id],
            |row| {
                Ok(PreviousAttempt {
                    execution: row.get(0)?,
                    failure: row.get(1)?,
                    attempt: row.get(2)?,
                    due_at: row.get(3)?,
                    delivered: row.get(4)?,
                })
            },
        )
        .optional()?)
}

/// Keep a pending attempt of `execution` or advance a delivered one under
/// the same policy.
fn next_attempt(
    previous: Option<PreviousAttempt>,
    execution: &ExecutionId,
    failure: TurnFailure,
    now: UnixTime,
    spread: f64,
) -> (u32, UnixTime) {
    let limited = |failure| failure == TurnFailure::RateLimited;
    let previous = previous.filter(|previous| {
        (previous.delivered || previous.execution == *execution)
            && limited(previous.failure) == limited(failure)
    });
    match previous {
        Some(previous) if !previous.delivered => (previous.attempt, previous.due_at),
        Some(previous) => {
            let attempt = previous.attempt.saturating_add(1);
            (attempt, now + delay(failure, attempt, spread))
        }
        _ => (1, now + delay(failure, 1, spread)),
    }
}

/// The next attempt of a space's retry, and the reset it waits past
/// when one is known.
struct Retry {
    failure: TurnFailure,
    attempt: u32,
    due_at: UnixTime,
    resets_at: Option<UnixTime>,
}

/// Persist the next attempt, replacing a previous one for the space.
fn save_retry(
    db: &Connection,
    space_id: &SpaceId,
    execution: &ExecutionId,
    retry: &Retry,
) -> Result<()> {
    db.execute(
        "INSERT INTO space_turn_retries(space_id,execution_id,failure,attempt,due_at,delivered,
                 resets_at)
             VALUES (?1,?2,?3,?4,?5,0,?6)
             ON CONFLICT(space_id) DO UPDATE SET execution_id=excluded.execution_id,
                 failure=excluded.failure,attempt=excluded.attempt,due_at=excluded.due_at,
                 delivered=0,resets_at=excluded.resets_at",
        params![
            space_id,
            execution,
            retry.failure,
            retry.attempt,
            retry.due_at,
            retry.resets_at
        ],
    )?;
    Ok(())
}

/// Drop the space's retry.
pub fn cancel(db: &Connection, space_id: &SpaceId) -> Result<()> {
    db.execute(
        "DELETE FROM space_turn_retries WHERE space_id=?1",
        [space_id],
    )?;
    Ok(())
}

/// A due retry, with whether it may be delivered: the reporting execution
/// still runs the idle agent, or a restore of it waits in the queue, which
/// the message wakes, and the assignment is not finished.
struct Due {
    failure: TurnFailure,
    attempt: u32,
    deliverable: bool,
}

/// The spaces whose retry is due at `now`.
fn due_spaces(db: &Connection, now: UnixTime) -> Result<Vec<SpaceId>> {
    Ok(db
        .prepare("SELECT space_id FROM space_turn_retries WHERE delivered=0 AND due_at<=?1")?
        .query_map([now], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// The space's retry when it is still due at `now`.
fn due(
    db: &Connection,
    records: &dyn Records,
    space_id: &SpaceId,
    now: UnixTime,
) -> Result<Option<Due>> {
    let due = db
        .query_row(
            "SELECT r.failure,r.attempt,
                s.state IS 'idle' AND s.execution_id IS r.execution_id
                    OR EXISTS (SELECT 1 FROM agent_queue q
                        WHERE q.kind=?3 AND q.execution_id=r.execution_id)
             FROM space_turn_retries r
             LEFT JOIN space_agent_state s ON s.space_id=r.space_id
             WHERE r.space_id=?1 AND r.delivered=0 AND r.due_at<=?2",
            params![space_id, now, QueueKind::Restore],
            |row| {
                Ok(Due {
                    failure: row.get(0)?,
                    attempt: row.get(1)?,
                    deliverable: row.get(2)?,
                })
            },
        )
        .optional()?;
    let Some(mut due) = due else {
        return Ok(None);
    };
    due.deliverable &= !records.space_done(db, space_id)?;
    Ok(Some(due))
}

/// What delivering the due retries did.
struct Delivered {
    /// Whether a message was queued.
    queued: bool,
    /// The spaces whose retry could not be delivered, with why; each is
    /// tried again after the first delay.
    failed: Vec<(SpaceId, anyhow::Error)>,
    /// When the next retry is due.
    next: Option<UnixTime>,
}

/// Deliver each retry due at `now`, each space's on its own, so one
/// that cannot be delivered, such as to a full message queue, holds up no
/// other and waits the first delay again.
fn deliver_due(db: &mut Connection, records: &dyn Records, now: UnixTime) -> Result<Delivered> {
    let mut queued = false;
    let mut failed = Vec::new();
    for space_id in due_spaces(db, now)? {
        match shoal_store::write_transaction(db, |tx| deliver(tx, records, &space_id, now)) {
            Ok(delivered) => queued |= delivered,
            Err(error) => {
                postpone(db, &space_id, now + Duration::from_secs(FIRST_DELAY_SECS))?;
                failed.push((space_id, error));
            }
        }
    }
    let next = db.query_row(
        "SELECT MIN(due_at) FROM space_turn_retries WHERE delivered=0",
        [],
        |row| row.get(0),
    )?;
    Ok(Delivered {
        queued,
        failed,
        next,
    })
}

/// Ask the agent to continue when the space's retry is still due at
/// `now` and may be delivered, queueing the message and marking the retry
/// delivered together, or drop a retry that may not be. One transaction
/// checks and delivers, so a turn that starts meanwhile never receives a
/// cancelled retry. Whether a message was queued.
fn deliver(
    tx: &Transaction<'_>,
    records: &dyn Records,
    space_id: &SpaceId,
    now: UnixTime,
) -> Result<bool> {
    let Some(due) = due(tx, records, space_id, now)? else {
        return Ok(false);
    };
    if due.deliverable {
        let message = continuation(due.failure, due.attempt);
        let kind = shoal_daemon_base::alerts::AlertKind::TurnRetried;
        messages::queue_shoal_message(tx, records, space_id, kind, &message)?;
        tx.execute(
            "UPDATE space_turn_retries SET delivered=1 WHERE space_id=?1",
            [space_id],
        )?;
    } else {
        cancel(tx, space_id)?;
    }
    Ok(due.deliverable)
}

fn postpone(db: &Connection, space_id: &SpaceId, due_at: UnixTime) -> Result<()> {
    db.execute(
        "UPDATE space_turn_retries SET due_at=?2 WHERE space_id=?1 AND delivered=0",
        params![space_id, due_at],
    )?;
    Ok(())
}

/// The message that asks the agent to continue.
pub fn continuation(failure: TurnFailure, attempt: u32) -> String {
    format!(
        "Shoal: your last turn ended with an API error ({}). Continue the unfinished \
         work from where you left off. (Retry {attempt}.)",
        failure.describe()
    )
}

impl Agents {
    /// Schedule the retry of the turn `failed` ended in `execution` when
    /// its agent has `retry` on and a retry fixes the failure, or tell of
    /// the turn Shoal does not retry.
    /// Failures only log, since the report still records the agent idle.
    pub(crate) async fn retry_failed_turn(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        failed: FailedTurn,
        execution: &ExecutionId,
    ) {
        if let Err(error) = self
            .schedule_turn_retry(daemon, selector, failed, execution)
            .await
        {
            log!("turn retry for {selector}: {error:#}");
        }
    }

    async fn schedule_turn_retry(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        failed: FailedTurn,
        execution: &ExecutionId,
    ) -> Result<()> {
        let Some(agent) = self.agent_name(execution).await else {
            return Ok(());
        };
        let space = daemon.space(selector).await?;
        let failure = failed.failure;
        if let Some(reason) = Unretried::of(failure, self.retries(daemon, &space, &agent).await) {
            return self
                .leave_failed_turn(daemon, &space, execution, &agent, failure, reason)
                .await;
        }
        let (id, execution) = (space.id.clone(), execution.to_owned());
        let (now, spread) = (UnixTime::now(), spread());
        self.store
            .write(move |tx| record_failure(tx, &id, &execution, failed, now, spread))
            .await?;
        self.signals.wake_turn_retry();
        Ok(())
    }

    /// Keep `reset`, which `execution`'s proxy reports as when the upstream
    /// refuses requests until, holding its pending turn retry until past
    /// it; `None` forgets it.
    pub async fn record_upstream_reset(
        &self,
        execution: ExecutionId,
        reset: Option<UnixTime>,
    ) -> Result<()> {
        if self
            .store
            .write(move |tx| record_reset(tx, &execution, reset))
            .await?
        {
            self.signals.wake_turn_retry();
        }
        Ok(())
    }

    /// The failure whose retry, not yet delivered, waits for the agent of
    /// the space `id`.
    pub async fn waiting_turn_retry(&self, id: &SpaceId) -> Result<Option<TurnFailure>> {
        let id = id.to_owned();
        let retry = self.store.read(move |tx| get(tx, &id)).await?;
        Ok(retry
            .filter(|retry| !retry.delivered)
            .map(|retry| retry.failure))
    }

    /// Whether the AI tool `agent` names has `retry` on in `space`.
    async fn retries(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        agent: &crate::agent::Agent,
    ) -> bool {
        daemon.space_settings(space).await.is_ok_and(|settings| {
            settings
                .tools
                .get(agent.name())
                .is_some_and(|tool| tool.retry)
        })
    }

    /// Deliver the retries due at `now`, as [`deliver_due`] does; when the
    /// next one is due.
    pub async fn deliver_turn_retries(&self, now: UnixTime) -> Result<Option<UnixTime>> {
        let records = self.records.clone();
        let delivered = (self.store)
            .run(move |db| deliver_due(db, &*records, now))
            .await?;
        for (workspace, error) in &delivered.failed {
            log!("turn retry for {workspace} not delivered: {error:#}");
        }
        if delivered.queued {
            self.signals.message_queued();
        }
        Ok(delivered.next)
    }
}

/// Deliver each retry when it is due, waking early when one is scheduled.
pub async fn run(agents: Arc<Agents>) {
    loop {
        let next = {
            let _operation = agents.gates.operation().await;
            agents.deliver_turn_retries(UnixTime::now()).await
        };
        let wait = match next {
            Ok(Some(due)) => due
                .duration_since(UnixTime::now())
                .max(Duration::from_secs(1)),
            Ok(None) => IDLE_RECHECK,
            Err(error) => {
                log!("turn retries: {error:#}");
                Duration::from_secs(FIRST_DELAY_SECS)
            }
        };
        tokio::select! {
            () = agents.signals.turn_retry_woken() => {}
            () = tokio::time::sleep(wait) => {}
        }
    }
}

#[cfg(test)]
mod tests;
