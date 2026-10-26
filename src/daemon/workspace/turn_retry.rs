//! Retrying a turn an API failure ended. An agent whose integration reports
//! failed turns, with `retry` on, is asked to continue once the failure has
//! had time to pass. The retry is persisted, so it outlasts a daemon
//! restart, and knows no agent: any report of a failed turn schedules one.
use std::{sync::Arc, time::Duration};

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

use super::Manager;
use crate::{
    daemon::{agent_messages, log, notifications::NotificationKind},
    state::{AgentState, TurnFailure},
};

/// Attempts at one turn before Shoal gives up on it; with the delays below
/// they span about an hour.
pub(crate) const ATTEMPTS: u32 = 8;
/// The wait before the first attempt, doubling for each later one.
const FIRST_DELAY_SECS: u64 = 30;
/// The longest wait before an attempt.
const MAX_DELAY_SECS: u64 = 15 * 60;
/// How far each wait is spread, as a fraction either way, so agents that
/// failed together do not all retry at once.
const JITTER: f64 = 0.2;
/// How often the loop looks again without a retry to wait for.
const IDLE_RECHECK: Duration = Duration::from_secs(60 * 60);

/// A workspace's retry of its agent's failed turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnRetry {
    pub failure: TurnFailure,
    /// The attempt the retry makes or made, from 1.
    pub attempt: u32,
    pub attempts: u32,
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
                    attempts: ATTEMPTS,
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

/// The wait before `attempt`, spread by `spread` in `[0, 1)`.
fn delay(attempt: u32, spread: f64) -> i64 {
    let doublings = attempt.saturating_sub(1).min(16);
    let base = FIRST_DELAY_SECS
        .saturating_mul(1 << doublings)
        .min(MAX_DELAY_SECS);
    (base as f64 * (1.0 - JITTER + 2.0 * JITTER * spread)).round() as i64
}

/// A random spread in `[0, 1)`.
fn spread() -> f64 {
    (uuid::Uuid::new_v4().as_u128() % 1_000_000) as f64 / 1_000_000.0
}

/// What reporting a failed turn did to its workspace's retry.
#[derive(Debug, PartialEq, Eq)]
enum Scheduled {
    /// The retry waits for its attempt.
    Waiting,
    /// The turn failed on its last attempt; the retry is gone.
    Exhausted,
}

/// Schedule the retry of the turn `failure` ended in `execution`: the next
/// attempt after a delivered one, the first for a new execution, and the
/// same one when the failure is reported again before it is due.
fn schedule(
    tx: &Transaction<'_>,
    workspace_id: &str,
    execution: &str,
    failure: TurnFailure,
    now: i64,
    spread: f64,
) -> Result<Scheduled> {
    let previous: Option<(String, u32, i64, bool)> = tx
        .query_row(
            "SELECT execution_id,attempt,due_at,delivered FROM workspace_turn_retries
             WHERE workspace_id=?1",
            [workspace_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let (attempt, due_at) = match previous {
        Some((previous, attempt, due_at, false)) if previous == execution => (attempt, due_at),
        Some((previous, attempt, _, true)) if previous == execution => {
            (attempt + 1, now + delay(attempt + 1, spread))
        }
        _ => (1, now + delay(1, spread)),
    };
    if attempt > ATTEMPTS {
        tx.execute(
            "DELETE FROM workspace_turn_retries WHERE workspace_id=?1",
            [workspace_id],
        )?;
        return Ok(Scheduled::Exhausted);
    }
    tx.execute(
        "INSERT INTO workspace_turn_retries(workspace_id,execution_id,failure,attempt,due_at,delivered)
         VALUES (?1,?2,?3,?4,?5,0)
         ON CONFLICT(workspace_id) DO UPDATE SET execution_id=excluded.execution_id,
             failure=excluded.failure,attempt=excluded.attempt,due_at=excluded.due_at,delivered=0",
        params![workspace_id, execution, failure, attempt, due_at],
    )?;
    Ok(Scheduled::Waiting)
}

/// A retry whose attempt is due, with the turn state its workspace reports.
struct Due {
    workspace_id: String,
    failure: TurnFailure,
    attempt: u32,
    /// Whether the reporting execution still runs the idle agent.
    idle: bool,
}

/// The message that asks the agent to continue.
fn continuation(failure: TurnFailure, attempt: u32) -> String {
    format!(
        "Shoal: your last turn ended with an API error ({}). Continue the unfinished \
         work from where you left off. (Retry {attempt} of {ATTEMPTS}.)",
        failure.describe()
    )
}

impl Manager {
    /// Schedule the retry of the turn `failure` ended in `execution` when
    /// its agent has `retry` on; a failure no retry fixes, or one on the
    /// last attempt, notifies instead. Failures only log, since the report
    /// still records the agent idle.
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
        if !crate::ai::retries(&self.config().ai, &agent) {
            return Ok(());
        }
        let workspace = self.workspace(selector).await?;
        if !failure.retryable() {
            self.cancel_turn_retry(&workspace.id).await?;
            let message = format!(
                "{agent}'s turn ended with an API error that a retry does not fix; check the agent"
            );
            self.notify(Some(&workspace.name), NotificationKind::TurnFailed, message)
                .await;
            return Ok(());
        }
        let (id, execution) = (workspace.id.clone(), execution.to_owned());
        let now = i64::try_from(crate::time::unix_seconds())?;
        let spread = spread();
        let scheduled = self
            .store
            .run(move |db| {
                let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                let scheduled = schedule(&tx, &id, &execution, failure, now, spread)?;
                tx.commit()?;
                Ok(scheduled)
            })
            .await?;
        if scheduled == Scheduled::Exhausted {
            let message = format!(
                "{agent}'s turn still fails ({}) after {ATTEMPTS} attempts; Shoal stopped retrying it",
                failure.describe()
            );
            self.notify(Some(&workspace.name), NotificationKind::TurnFailed, message)
                .await;
        }
        self.turn_retry_wake.notify_one();
        Ok(())
    }

    async fn cancel_turn_retry(&self, workspace_id: &str) -> Result<()> {
        let workspace_id = workspace_id.to_owned();
        self.store
            .run(move |db| {
                db.execute(
                    "DELETE FROM workspace_turn_retries WHERE workspace_id=?1",
                    [workspace_id],
                )?;
                Ok(())
            })
            .await
    }

    /// Ask each agent whose retry is due to continue, dropping a retry whose
    /// agent is no longer the reporting one idle or whose assignment is
    /// finished; the time the next one is due.
    pub(crate) async fn deliver_turn_retries(&self) -> Result<Option<i64>> {
        let now = i64::try_from(crate::time::unix_seconds())?;
        for due in self.due_turn_retries(now).await? {
            let deliver = due.idle && self.completion(&due.workspace_id).await?.is_none();
            self.deliver_turn_retry(due, deliver).await?;
        }
        self.store
            .run(|db| {
                Ok(db.query_row(
                    "SELECT MIN(due_at) FROM workspace_turn_retries WHERE delivered=0",
                    [],
                    |row| row.get(0),
                )?)
            })
            .await
    }

    async fn due_turn_retries(&self, now: i64) -> Result<Vec<Due>> {
        self.store
            .run(move |db| {
                Ok(db
                    .prepare(
                        "SELECT r.workspace_id,r.failure,r.attempt,
                            s.state IS 'idle' AND s.execution_id IS r.execution_id
                         FROM workspace_turn_retries r
                         LEFT JOIN workspace_agent_state s ON s.workspace_id=r.workspace_id
                         WHERE r.delivered=0 AND r.due_at<=?1",
                    )?
                    .query_map([now], |row| {
                        Ok(Due {
                            workspace_id: row.get(0)?,
                            failure: row.get(1)?,
                            attempt: row.get(2)?,
                            idle: row.get(3)?,
                        })
                    })?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
    }

    /// Queue the continuation and mark the retry delivered together, or drop
    /// the retry when it may not be `deliver`ed.
    async fn deliver_turn_retry(&self, due: Due, deliver: bool) -> Result<()> {
        let queued = self
            .store
            .run(move |db| {
                let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                if !deliver {
                    tx.execute(
                        "DELETE FROM workspace_turn_retries WHERE workspace_id=?1 AND delivered=0",
                        [&due.workspace_id],
                    )?;
                    tx.commit()?;
                    return Ok(false);
                }
                let message = continuation(due.failure, due.attempt);
                agent_messages::insert(&tx, &due.workspace_id, &message)?;
                tx.execute(
                    "UPDATE workspace_turn_retries SET delivered=1
                     WHERE workspace_id=?1 AND attempt=?2 AND delivered=0",
                    params![due.workspace_id, due.attempt],
                )?;
                tx.commit()?;
                Ok(true)
            })
            .await?;
        if queued {
            self.agent_messages_changed.send_modify(|count| *count += 1);
        }
        Ok(())
    }
}

/// Deliver each retry when it is due, waking early when one is scheduled.
pub(crate) async fn run(manager: Arc<Manager>) {
    loop {
        let next = {
            let _operation = manager.background_operations.read().await;
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
            () = manager.turn_retry_wake.notified() => {}
            () = tokio::time::sleep(wait) => {}
        }
    }
}

#[cfg(test)]
mod tests;
