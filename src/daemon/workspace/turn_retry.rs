//! Retrying a turn an API failure ended. An agent whose integration reports
//! failed turns, with `retry` on, is asked to continue once the failure has
//! had time to pass, or once the reset its upstream named has passed, and
//! again after each turn that still fails, until one succeeds. The retry is
//! persisted, so it outlasts a daemon restart, and knows no agent: any
//! report of a failed turn schedules one, or tells of a turn Shoal does not
//! retry.
use std::{sync::Arc, time::Duration};

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use self::unretried::Unretried;
use super::Manager;
use crate::{
    daemon::{agent_messages, log},
    state::{AgentState, TurnFailure},
};

pub(super) mod unretried;

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
const RESET_MARGIN_SECS: i64 = 30;
/// How often the loop looks again without a retry to wait for.
const IDLE_RECHECK: Duration = Duration::from_secs(60 * 60);

/// A workspace's retry of its agent's failed turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnRetry {
    pub failure: TurnFailure,
    /// The attempt the retry makes or made, from 1.
    pub attempt: u32,
    /// Unix seconds when the agent is asked to continue.
    pub due_at: i64,
    /// Whether the agent was asked and its turn has not ended yet.
    pub delivered: bool,
}

pub(crate) fn get(db: &Connection, workspace_id: &str) -> Result<Option<TurnRetry>> {
    Ok(db
        .query_row(
            "SELECT failure,attempt,due_at,delivered FROM workspace_turn_retries
             WHERE workspace_id=?1",
            [workspace_id],
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
pub(super) fn follow_state(db: &Connection, workspace_id: &str, state: AgentState) -> Result<()> {
    let delivered = match state {
        AgentState::Working => false,
        AgentState::Idle => true,
        AgentState::Waiting => return Ok(()),
    };
    db.execute(
        "DELETE FROM workspace_turn_retries WHERE workspace_id=?1 AND delivered=?2",
        params![workspace_id, delivered],
    )?;
    Ok(())
}

/// The wait before `attempt` for `failure`, spread by `spread` in `[0, 1)`.
fn delay(failure: TurnFailure, attempt: u32, spread: f64) -> i64 {
    let maximum = match failure {
        TurnFailure::RateLimited => RATE_LIMIT_MAX_DELAY_SECS,
        _ => MAX_DELAY_SECS,
    };
    let doublings = attempt.saturating_sub(1).min(16);
    let base = FIRST_DELAY_SECS.saturating_mul(1 << doublings).min(maximum);
    (base as f64 * (1.0 - JITTER + 2.0 * JITTER * spread)).round() as i64
}

/// A random spread in `[0, 1)`.
fn spread() -> f64 {
    (uuid::Uuid::new_v4().as_u128() % 1_000_000) as f64 / 1_000_000.0
}

/// Record the turn `failure` ended in `execution` as its workspace's
/// retry: the next attempt after a delivered one, the first for a new
/// execution or a change between usage limits and transient failures, and the
/// same one when the failure is reported again before it is due. An attempt
/// waits past the reset the execution's proxy last reported, read in the
/// same transaction as a report of it, so neither can slip between.
fn record_failure(
    db: &mut Connection,
    workspace_id: &str,
    execution: &str,
    failure: TurnFailure,
    now: i64,
    spread: f64,
) -> Result<()> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let reset = upstream_reset(&tx, execution)?;
    let previous = previous_attempt(&tx, workspace_id)?;
    let (attempt, due_at) = next_attempt(previous, execution, failure, now, spread);
    let due_at = reset.map_or(due_at, |reset| {
        due_at.max(reset.saturating_add(RESET_MARGIN_SECS))
    });
    save_retry(&tx, workspace_id, execution, failure, attempt, due_at)?;
    tx.commit()?;
    Ok(())
}

/// Keep `reset`, when the upstream of `execution`'s proxy refuses requests
/// until, and hold its pending turn retry until past it, together, so a
/// failure recorded at the same time sees one or the other; `None` forgets
/// it. Whether a pending retry was held.
fn record_reset(db: &mut Connection, execution: &str, reset: Option<i64>) -> Result<bool> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let Some(reset) = reset else {
        forget_upstream_reset(&tx, execution)?;
        tx.commit()?;
        return Ok(false);
    };
    tx.execute(
        "INSERT INTO execution_upstream_resets(execution_id,resets_at) VALUES (?1,?2)
         ON CONFLICT(execution_id) DO UPDATE SET resets_at=excluded.resets_at",
        params![execution, reset],
    )?;
    let held = tx.execute(
        "UPDATE workspace_turn_retries SET due_at=MAX(due_at,?2)
         WHERE execution_id=?1 AND delivered=0",
        params![execution, reset.saturating_add(RESET_MARGIN_SECS)],
    )?;
    tx.commit()?;
    Ok(held > 0)
}

/// When the upstream of `execution`'s proxy last said it refuses requests
/// until.
fn upstream_reset(db: &Connection, execution: &str) -> Result<Option<i64>> {
    Ok(db
        .query_row(
            "SELECT resets_at FROM execution_upstream_resets WHERE execution_id=?1",
            [execution],
            |row| row.get(0),
        )
        .optional()?)
}

/// Forget the reset `execution`'s proxy reported.
fn forget_upstream_reset(db: &Connection, execution: &str) -> Result<()> {
    db.execute(
        "DELETE FROM execution_upstream_resets WHERE execution_id=?1",
        [execution],
    )?;
    Ok(())
}

/// The persisted attempt whose execution and policy determine its successor.
struct PreviousAttempt {
    execution: String,
    failure: TurnFailure,
    attempt: u32,
    due_at: i64,
    delivered: bool,
}

/// Decode the workspace's most recent attempt, when one is recorded.
fn previous_attempt(db: &Connection, workspace_id: &str) -> Result<Option<PreviousAttempt>> {
    Ok(db
        .query_row(
            "SELECT execution_id,failure,attempt,due_at,delivered FROM workspace_turn_retries
             WHERE workspace_id=?1",
            [workspace_id],
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

/// Keep a pending attempt or advance a delivered one under the same policy.
fn next_attempt(
    previous: Option<PreviousAttempt>,
    execution: &str,
    failure: TurnFailure,
    now: i64,
    spread: f64,
) -> (u32, i64) {
    let limited = |failure| failure == TurnFailure::RateLimited;
    let previous = previous.filter(|previous| {
        previous.execution == execution && limited(previous.failure) == limited(failure)
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

/// Persist the next attempt, replacing a previous one for the workspace.
fn save_retry(
    db: &Connection,
    workspace_id: &str,
    execution: &str,
    failure: TurnFailure,
    attempt: u32,
    due_at: i64,
) -> Result<()> {
    db.execute(
        "INSERT INTO workspace_turn_retries(workspace_id,execution_id,failure,attempt,due_at,delivered)
             VALUES (?1,?2,?3,?4,?5,0)
             ON CONFLICT(workspace_id) DO UPDATE SET execution_id=excluded.execution_id,
                 failure=excluded.failure,attempt=excluded.attempt,due_at=excluded.due_at,delivered=0",
        params![workspace_id, execution, failure, attempt, due_at],
    )?;
    Ok(())
}

/// Drop the workspace's retry.
pub(super) fn cancel(db: &Connection, workspace_id: &str) -> Result<()> {
    db.execute(
        "DELETE FROM workspace_turn_retries WHERE workspace_id=?1",
        [workspace_id],
    )?;
    Ok(())
}

/// A due retry, with whether it may be delivered: the reporting execution
/// still runs the idle agent and the assignment is not finished.
struct Due {
    failure: TurnFailure,
    attempt: u32,
    deliverable: bool,
}

/// The workspaces whose retry is due at `now`.
fn due_workspaces(db: &Connection, now: i64) -> Result<Vec<String>> {
    Ok(db
        .prepare(
            "SELECT workspace_id FROM workspace_turn_retries WHERE delivered=0 AND due_at<=?1",
        )?
        .query_map([now], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// The workspace's retry when it is still due at `now`.
fn due(db: &Connection, workspace_id: &str, now: i64) -> Result<Option<Due>> {
    Ok(db
        .query_row(
            "SELECT r.failure,r.attempt,
                s.state IS 'idle' AND s.execution_id IS r.execution_id
                AND NOT EXISTS (SELECT 1 FROM workspace_completion c
                    WHERE c.workspace_id=r.workspace_id)
             FROM workspace_turn_retries r
             LEFT JOIN workspace_agent_state s ON s.workspace_id=r.workspace_id
             WHERE r.workspace_id=?1 AND r.delivered=0 AND r.due_at<=?2",
            params![workspace_id, now],
            |row| {
                Ok(Due {
                    failure: row.get(0)?,
                    attempt: row.get(1)?,
                    deliverable: row.get(2)?,
                })
            },
        )
        .optional()?)
}

/// What delivering the due retries did.
struct Delivered {
    /// Whether a message was queued.
    queued: bool,
    /// The workspaces whose retry could not be delivered, with why; each is
    /// tried again after the first delay.
    failed: Vec<(String, anyhow::Error)>,
    /// When the next retry is due.
    next: Option<i64>,
}

/// Deliver each retry due at `now`, each workspace's on its own, so one
/// that cannot be delivered, such as to a full message queue, holds up no
/// other and waits the first delay again.
fn deliver_due(db: &mut Connection, now: i64) -> Result<Delivered> {
    let mut queued = false;
    let mut failed = Vec::new();
    for workspace_id in due_workspaces(db, now)? {
        match deliver(db, &workspace_id, now) {
            Ok(delivered) => queued |= delivered,
            Err(error) => {
                postpone(db, &workspace_id, now + FIRST_DELAY_SECS as i64)?;
                failed.push((workspace_id, error));
            }
        }
    }
    let next = db.query_row(
        "SELECT MIN(due_at) FROM workspace_turn_retries WHERE delivered=0",
        [],
        |row| row.get(0),
    )?;
    Ok(Delivered {
        queued,
        failed,
        next,
    })
}

/// Ask the agent to continue when the workspace's retry is still due at
/// `now` and may be delivered, queueing the message and marking the retry
/// delivered together, or drop a retry that may not be. One transaction
/// checks and delivers, so a turn that starts meanwhile never receives a
/// cancelled retry. Whether a message was queued.
fn deliver(db: &mut Connection, workspace_id: &str, now: i64) -> Result<bool> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let Some(due) = due(&tx, workspace_id, now)? else {
        return Ok(false);
    };
    if due.deliverable {
        agent_messages::insert(
            &tx,
            workspace_id,
            &continuation(due.failure, due.attempt),
            &[],
        )?;
        tx.execute(
            "UPDATE workspace_turn_retries SET delivered=1 WHERE workspace_id=?1",
            [workspace_id],
        )?;
    } else {
        cancel(&tx, workspace_id)?;
    }
    tx.commit()?;
    Ok(due.deliverable)
}

fn postpone(db: &Connection, workspace_id: &str, due_at: i64) -> Result<()> {
    db.execute(
        "UPDATE workspace_turn_retries SET due_at=?2 WHERE workspace_id=?1 AND delivered=0",
        params![workspace_id, due_at],
    )?;
    Ok(())
}

/// The message that asks the agent to continue.
fn continuation(failure: TurnFailure, attempt: u32) -> String {
    format!(
        "Shoal: your last turn ended with an API error ({}). Continue the unfinished \
         work from where you left off. (Retry {attempt}.)",
        failure.describe()
    )
}

impl Manager {
    /// Schedule the retry of the turn `failure` ended in `execution` when
    /// its agent has `retry` on and a retry fixes the failure, or tell of
    /// the turn Shoal does not retry.
    /// Failures only log, since the report still records the agent idle.
    pub(super) async fn retry_failed_turn(
        &self,
        selector: &str,
        failure: TurnFailure,
        execution: &str,
    ) {
        if let Err(error) = self.schedule_turn_retry(selector, failure, execution).await {
            log!("turn retry for {selector}: {error:#}");
        }
    }

    async fn schedule_turn_retry(
        &self,
        selector: &str,
        failure: TurnFailure,
        execution: &str,
    ) -> Result<()> {
        let Some(agent) = self.agent_name(execution).await else {
            return Ok(());
        };
        let workspace = self.workspace(selector).await?;
        if let Some(reason) = Unretried::of(failure, self.retries(&workspace, &agent).await) {
            return self
                .leave_failed_turn(&workspace, execution, &agent, failure, reason)
                .await;
        }
        let (id, execution) = (workspace.id.clone(), execution.to_owned());
        let (now, spread) = (i64::try_from(crate::time::unix_seconds())?, spread());
        self.store
            .run(move |db| record_failure(db, &id, &execution, failure, now, spread))
            .await?;
        self.signals.wake_turn_retry();
        Ok(())
    }

    /// Keep `reset`, which `execution`'s proxy reports as when the upstream
    /// refuses requests until, holding its pending turn retry until past
    /// it; `None` forgets it.
    pub(super) async fn record_upstream_reset(
        &self,
        execution: String,
        reset: Option<i64>,
    ) -> Result<()> {
        if self
            .store
            .run(move |db| record_reset(db, &execution, reset))
            .await?
        {
            self.signals.wake_turn_retry();
        }
        Ok(())
    }

    /// Whether the AI tool `agent` names has `retry` on in `workspace`.
    async fn retries(&self, workspace: &crate::model::Workspace, agent: &str) -> bool {
        self.workspace_settings(workspace)
            .await
            .is_ok_and(|settings| settings.tools.get(agent).is_some_and(|tool| tool.retry))
    }

    /// Deliver the retries due now; when the next one is due.
    pub(crate) async fn deliver_turn_retries(&self) -> Result<Option<i64>> {
        self.deliver_turn_retries_at(i64::try_from(crate::time::unix_seconds())?)
            .await
    }

    /// Deliver the retries due at `now`, as [`deliver_due`] does.
    async fn deliver_turn_retries_at(&self, now: i64) -> Result<Option<i64>> {
        let delivered = self.store.run(move |db| deliver_due(db, now)).await?;
        for (workspace, error) in &delivered.failed {
            log!("turn retry for {workspace} not delivered: {error:#}");
        }
        if delivered.queued {
            self.signals.agent_message_queued();
        }
        Ok(delivered.next)
    }
}

/// Deliver each retry when it is due, waking early when one is scheduled.
pub(crate) async fn run(manager: Arc<Manager>) {
    loop {
        let next = {
            let _operation = manager.gates.operation().await;
            manager.deliver_turn_retries().await
        };
        let wait = match next {
            Ok(Some(due)) => {
                let now = i64::try_from(crate::time::unix_seconds()).unwrap_or(i64::MAX);
                Duration::from_secs(u64::try_from(due.saturating_sub(now)).unwrap_or(0).max(1))
            }
            Ok(None) => IDLE_RECHECK,
            Err(error) => {
                log!("turn retries: {error:#}");
                Duration::from_secs(FIRST_DELAY_SECS)
            }
        };
        tokio::select! {
            () = manager.signals.turn_retry_woken() => {}
            () = tokio::time::sleep(wait) => {}
        }
    }
}

#[cfg(test)]
mod tests;
