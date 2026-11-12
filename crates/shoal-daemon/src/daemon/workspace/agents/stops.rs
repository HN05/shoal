//! The protection stops Shoal decides for agents, which the store keeps by
//! the execution that runs each, beside the turn state the agent reports;
//! the tracked agent only carries a stop to its wrapper.
use crate::client::execution_ids::ExecutionId;
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashMap;

use super::{Agent, Manager, Protection, ProtectionExt, restart};
use crate::daemon::log;
use crate::{state::AgentState, time::UnixTime};
use std::path::{Path, PathBuf};

/// The stop recorded for execution `id`.
fn get(db: &Connection, id: &ExecutionId) -> Result<Option<Protection>> {
    Ok(db
        .query_row(
            "SELECT reason,resumes_when FROM agent_stops WHERE execution_id=?1",
            [id],
            |row| {
                Ok(Protection {
                    reason: row.get(0)?,
                    resumes_when: row.get(1)?,
                })
            },
        )
        .optional()?)
}

/// Every recorded stop, by execution.
pub(super) fn all(db: &Connection) -> Result<HashMap<ExecutionId, Protection>> {
    Ok(db
        .prepare("SELECT execution_id,reason,resumes_when FROM agent_stops")?
        .query_map([], |row| {
            let protection = Protection {
                reason: row.get(1)?,
                resumes_when: row.get(2)?,
            };
            Ok((row.get(0)?, protection))
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// What a stop saves for the agent's restore before it is delivered: the
/// reason and turn its recovery record keeps, and whether the stop goes only
/// with it saved.
pub(crate) struct Handoff<'a> {
    pub reason: &'a str,
    pub turn: Option<AgentState>,
    pub required: bool,
}

/// Remove the recovery handoff of a stop that could not be delivered.
fn discard_handoff(handoff: Option<&Path>) {
    if let Some(handoff) = handoff
        && let Err(error) = crate::execution::recovery::consume(handoff)
    {
        log!("unused recovery handoff not removed: {error:#}");
    }
}

/// What still holds when a stop chosen between turns is recorded: the agent
/// is idle in the turn generation it was chosen in, and for a stop that
/// frees memory, its prompt cache expired by `cache_expired_at`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Between {
    pub turn: i64,
    pub cache_expired_at: Option<UnixTime>,
}

/// Whether the agent of execution `id` is still `between` turns: a turn
/// that started since, or a turn end that renewed its cache, keeps it.
fn still_between(db: &Connection, id: &ExecutionId, between: Between) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS (SELECT 1 FROM space_agent_state
             WHERE execution_id=?1 AND state=?2 AND turn=?3
                 AND (?4 IS NULL OR prompt_cache_expires_at IS NULL
                     OR prompt_cache_expires_at<=?4))",
        params![id, AgentState::Idle, between.turn, between.cache_expired_at],
        |row| row.get(0),
    )?)
}

/// Record that `protection` stops the agent of execution `id` at its
/// space's current turn generation, unless a stop of it is recorded
/// already, or one chosen `between` turns finds that no longer holds.
/// Whether it was recorded.
pub(super) fn insert(
    db: &Connection,
    id: &ExecutionId,
    protection: &Protection,
    between: Option<Between>,
) -> Result<bool> {
    if let Some(between) = between
        && !still_between(db, id, between)?
    {
        return Ok(false);
    }
    let inserted = db.execute(
        "INSERT INTO agent_stops(execution_id,reason,resumes_when,turn)
         VALUES (?1,?2,?3,(SELECT turn FROM space_agent_state WHERE execution_id=?1))
         ON CONFLICT(execution_id) DO NOTHING",
        params![id, protection.reason, protection.resumes_when],
    )?;
    Ok(inserted > 0)
}

/// Record that `protection` stopped the agent of execution `id`, as its
/// reattaching wrapper reports it, in place of the stop recorded before.
fn set(db: &Connection, id: &ExecutionId, protection: &Protection) -> Result<()> {
    db.execute(
        "INSERT INTO agent_stops(execution_id,reason,resumes_when,turn)
         VALUES (?1,?2,?3,(SELECT turn FROM space_agent_state WHERE execution_id=?1))
         ON CONFLICT(execution_id) DO UPDATE SET
             reason=excluded.reason,resumes_when=excluded.resumes_when",
        params![id, protection.reason, protection.resumes_when],
    )?;
    Ok(())
}

/// Forget the stop recorded for execution `id`.
pub(super) fn delete(db: &Connection, id: &ExecutionId) -> Result<()> {
    db.execute("DELETE FROM agent_stops WHERE execution_id=?1", [id])?;
    Ok(())
}

/// Forget the stop of the agent of execution `id` as it runs again, with
/// the turn state it reported before, so nothing counts it idle in a turn
/// it ended before its stop until it reports its own; its space's turn
/// count continues.
fn resume(db: &Connection, id: &ExecutionId) -> Result<()> {
    delete(db, id)?;
    db.execute("DELETE FROM space_agent_state WHERE execution_id=?1", [id])?;
    Ok(())
}

/// Whether a stop is recorded for execution `id`, which a report from it
/// that starts a turn defers to.
pub(crate) fn recorded(db: &Connection, id: &ExecutionId) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS (SELECT 1 FROM agent_stops WHERE execution_id=?1)",
        [id],
        |row| row.get(0),
    )?)
}

impl Manager {
    /// The protection stop whose restore the tracked agent of `id` awaits:
    /// a stop request, as from `shoal stop` or a daemon shutdown, cancels
    /// it, and one of an execution no agent connected for authorizes
    /// nothing.
    pub(crate) async fn agent_overload(&self, id: &ExecutionId) -> Option<Protection> {
        if (self.executions.with_agent(id, |_| ()).await).is_none()
            || self.executions.stop_request(id).await.is_some()
        {
            return None;
        }
        let execution = id.to_owned();
        let recorded = self.store.run(move |db| get(db, &execution)).await;
        recorded.unwrap_or_else(|error| {
            log!("stop of execution {id} unreadable: {error:#}");
            None
        })
    }

    pub(crate) async fn agent_was_overloaded(&self, id: &ExecutionId) -> bool {
        self.agent_overload(id).await.is_some()
    }

    /// Mark each of `executions` whose agent protection stopped while its
    /// wrapper waits to restore it.
    pub(crate) async fn annotate_stops(&self, executions: &mut [crate::model::Execution]) {
        for execution in executions {
            execution.stopped = self.agent_overload(&execution.id).await;
        }
    }

    /// Record that `protection` stops the agent of `id`, chosen `between`
    /// turns when given, as [`insert`] decides. Whether it was recorded.
    pub(super) async fn record_agent_stop(
        &self,
        id: &ExecutionId,
        protection: &Protection,
        between: Option<Between>,
    ) -> Result<bool> {
        let (execution, protection) = (id.to_owned(), protection.clone());
        self.store
            .write(move |tx| insert(tx, &execution, &protection, between))
            .await
    }

    /// Forget the stop of the agent of `id` and its turn state as it runs
    /// again, as [`resume`] does.
    pub(super) async fn forget_resumed_stop(&self, id: &ExecutionId) -> Result<()> {
        let execution = id.to_owned();
        self.store.write(move |tx| resume(tx, &execution)).await
    }

    /// Forget the stop of the agent of `id`, as when it runs again or its
    /// restore is cancelled.
    pub(crate) async fn forget_agent_stop(&self, id: &ExecutionId) {
        let execution = id.to_owned();
        if let Err(error) = self.store.run(move |db| delete(db, &execution)).await {
            log!("stop of execution {id} not forgotten: {error:#}");
        }
    }

    /// Send the stop recorded for the agent of `id` to its wrapper, unless
    /// something else stops it meanwhile, such as `shoal restart`, saving
    /// its `handoff` first, so a wrapper terminated before it saves its own
    /// can still be resumed, and holding back a daemon update until the
    /// wrapper's connection takes over. Both happen under the lock every
    /// stop of the agent takes, so no other stop overwrites or removes the
    /// handoff meanwhile. Whether the handoff was saved.
    pub(super) async fn deliver_stop(
        &self,
        id: &ExecutionId,
        handoff: Handoff<'_>,
    ) -> Result<bool> {
        self.executions
            .with_agent(id, |agent| {
                anyhow::ensure!(
                    agent.running() && agent.restarts.current.is_none(),
                    "{} is already stopping",
                    agent.name
                );
                let saved = self.save_handoff(id, agent, &handoff)?;
                // The monitor holds an operation, so no update can take the lock.
                agent.update_hold = self.gates.try_owned_operation();
                if let Err(error) = agent.stop.send(true) {
                    discard_handoff(saved.as_deref());
                    return Err(error).with_context(|| format!("{} disconnected", agent.name));
                }
                Ok(saved.is_some())
            })
            .await
            .context("the agent disconnected")?
    }

    /// Save the recovery `handoff` of `agent`, the agent of `id`: its path,
    /// or none when it could not be saved and the stop goes without one.
    fn save_handoff(
        &self,
        id: &ExecutionId,
        agent: &Agent,
        handoff: &Handoff<'_>,
    ) -> Result<Option<PathBuf>> {
        let saved = crate::execution::recovery::save_record(
            &self.paths,
            &agent.workspace_id,
            id,
            &agent.tracked(),
            Some(handoff.reason),
            handoff.turn,
        )
        .context("save the agent's recovery record");
        match saved {
            Ok(path) => Ok(Some(path)),
            Err(error) if handoff.required => Err(error),
            Err(error) => {
                log!("recovery handoff not saved: {error:#}");
                Ok(None)
            }
        }
    }

    /// Restore the protection stop of an agent whose wrapper reattached
    /// while waiting for its automatic restore. A restart continues without
    /// the request that asked for it, which ended with the previous daemon,
    /// in a new wrapper.
    pub(crate) async fn await_agent_restore(&self, id: &ExecutionId, protection: Protection) {
        if protection.is_restart() {
            self.executions
                .with_agent(id, |agent| {
                    agent.restarts.current = Some(restart::Restart::unanswered(None, false));
                })
                .await;
            return;
        }
        let execution = id.to_owned();
        let recorded = (self.store)
            .run(move |db| set(db, &execution, &protection))
            .await;
        if let Err(error) = recorded {
            log!("stop of reattached execution {id} not recorded: {error:#}");
        }
    }
}

#[cfg(test)]
mod tests;
