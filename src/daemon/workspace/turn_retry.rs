//! Retrying a turn an API failure ended. An agent whose integration reports
//! failed turns, with `retry` on, is asked to continue once the failure has
//! had time to pass. The retry is persisted, so it outlasts a daemon
//! restart, and knows no agent: any report of a failed turn schedules one.
use std::{sync::Arc, time::Duration};

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use super::Manager;
use crate::{
    daemon::{agent_messages, log, notifications::NotificationKind},
    model::Workspace,
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

/// Why Shoal stops retrying a turn.
enum GiveUp {
    /// A retry does not fix the failure.
    Unfixable,
    /// The turn failed on its last attempt.
    Exhausted(TurnFailure),
}

/// Record the turn `failure` ended in `execution` as its workspace's
/// retry: the next attempt after a delivered one, the first for a new
/// execution, and the same one when the failure is reported again before it
/// is due.
fn record_failure(
    db: &mut Connection,
    workspace_id: &str,
    execution: &str,
    failure: TurnFailure,
    now: i64,
    spread: f64,
) -> Result<Scheduled> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
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
    let scheduled = if attempt > ATTEMPTS {
        remove(&tx, workspace_id)?;
        Scheduled::Exhausted
    } else {
        tx.execute(
            "INSERT INTO workspace_turn_retries(workspace_id,execution_id,failure,attempt,due_at,delivered)
             VALUES (?1,?2,?3,?4,?5,0)
             ON CONFLICT(workspace_id) DO UPDATE SET execution_id=excluded.execution_id,
                 failure=excluded.failure,attempt=excluded.attempt,due_at=excluded.due_at,delivered=0",
            params![workspace_id, execution, failure, attempt, due_at],
        )?;
        Scheduled::Waiting
    };
    tx.commit()?;
    Ok(scheduled)
}

fn remove(db: &Connection, workspace_id: &str) -> Result<()> {
    db.execute(
        "DELETE FROM workspace_turn_retries WHERE workspace_id=?1",
        [workspace_id],
    )?;
    Ok(())
}

/// A retry whose attempt is due at `now`, with whether it may be delivered:
/// the reporting execution still runs the idle agent and the assignment is
/// not finished.
struct Due {
    workspace_id: String,
    failure: TurnFailure,
    attempt: u32,
    deliverable: bool,
}

fn due(db: &Connection, now: i64) -> Result<Vec<Due>> {
    Ok(db
        .prepare(
            "SELECT r.workspace_id,r.failure,r.attempt,
                s.state IS 'idle' AND s.execution_id IS r.execution_id
                AND NOT EXISTS (SELECT 1 FROM workspace_completion c
                    WHERE c.workspace_id=r.workspace_id)
             FROM workspace_turn_retries r
             LEFT JOIN workspace_agent_state s ON s.workspace_id=r.workspace_id
             WHERE r.delivered=0 AND r.due_at<=?1",
        )?
        .query_map([now], |row| {
            Ok(Due {
                workspace_id: row.get(0)?,
                failure: row.get(1)?,
                attempt: row.get(2)?,
                deliverable: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// Ask each agent whose retry is due at `now` and may be delivered to
/// continue, queueing its message and marking the retry delivered together,
/// and drop the other due ones. One transaction checks and delivers, so a
/// turn that starts meanwhile never receives a cancelled retry. Whether a
/// message was queued, and when the next retry is due.
fn deliver_due(db: &mut Connection, now: i64) -> Result<(bool, Option<i64>)> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut queued = false;
    for due in due(&tx, now)? {
        if !due.deliverable {
            remove(&tx, &due.workspace_id)?;
            continue;
        }
        agent_messages::insert(
            &tx,
            &due.workspace_id,
            &continuation(due.failure, due.attempt),
        )?;
        tx.execute(
            "UPDATE workspace_turn_retries SET delivered=1 WHERE workspace_id=?1",
            [&due.workspace_id],
        )?;
        queued = true;
    }
    let next = tx.query_row(
        "SELECT MIN(due_at) FROM workspace_turn_retries WHERE delivered=0",
        [],
        |row| row.get(0),
    )?;
    tx.commit()?;
    Ok((queued, next))
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
        let Some(agent) = self.retrying_agent(execution).await else {
            return Ok(());
        };
        let workspace = self.workspace(selector).await?;
        if !failure.retryable() {
            return self
                .give_up_turn(&workspace, &agent, GiveUp::Unfixable)
                .await;
        }
        let (id, execution) = (workspace.id.clone(), execution.to_owned());
        let (now, spread) = (i64::try_from(crate::time::unix_seconds())?, spread());
        let scheduled = self
            .store
            .run(move |db| record_failure(db, &id, &execution, failure, now, spread))
            .await?;
        match scheduled {
            Scheduled::Waiting => self.turn_retry_wake.notify_one(),
            Scheduled::Exhausted => {
                let reason = GiveUp::Exhausted(failure);
                self.give_up_turn(&workspace, &agent, reason).await?;
            }
        }
        Ok(())
    }

    /// The name of the agent `execution` runs when its AI tool has `retry`
    /// on.
    async fn retrying_agent(&self, execution: &str) -> Option<String> {
        let agent = self.agent_name(execution).await?;
        crate::ai::retries(&self.config().ai, &agent).then_some(agent)
    }

    /// Drop the workspace's retry and tell whoever receives its agent's
    /// events why Shoal stops retrying.
    async fn give_up_turn(&self, workspace: &Workspace, agent: &str, reason: GiveUp) -> Result<()> {
        let id = workspace.id.clone();
        self.store.run(move |db| remove(db, &id)).await?;
        let message = match reason {
            GiveUp::Unfixable => format!(
                "{agent}'s turn ended with an API error that a retry does not fix; check the agent"
            ),
            GiveUp::Exhausted(failure) => format!(
                "{agent}'s turn still fails ({}) after {ATTEMPTS} attempts; Shoal stopped retrying it",
                failure.describe()
            ),
        };
        self.notify(Some(&workspace.name), NotificationKind::TurnFailed, message)
            .await;
        Ok(())
    }

    /// Deliver the retries due now; when the next one is due.
    pub(crate) async fn deliver_turn_retries(&self) -> Result<Option<i64>> {
        self.deliver_turn_retries_at(i64::try_from(crate::time::unix_seconds())?)
            .await
    }

    /// Deliver the retries due at `now`, as [`deliver_due`] does.
    async fn deliver_turn_retries_at(&self, now: i64) -> Result<Option<i64>> {
        let (queued, next) = self.store.run(move |db| deliver_due(db, now)).await?;
        if queued {
            self.agent_messages_changed.send_modify(|count| *count += 1);
        }
        Ok(next)
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
