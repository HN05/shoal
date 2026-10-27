//! The turn state agents report from their hooks: a status signal for
//! integrations that never notifies, completes, or changes cleanup.
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};

use super::Manager;
use crate::{
    execution::recovery,
    model::AgentStatus,
    state::{AgentState, TurnFailure},
};

pub(crate) fn get(db: &Connection, id: &str) -> Result<Option<AgentStatus>> {
    Ok(db
        .query_row(
            "SELECT state,since FROM workspace_agent_state WHERE workspace_id=?1",
            [id],
            |row| {
                Ok(AgentStatus {
                    state: row.get(0)?,
                    since: row.get(1)?,
                })
            },
        )
        .optional()?)
}

/// Every workspace's last reported state.
pub(crate) fn all(db: &Connection) -> Result<Vec<(String, AgentState)>> {
    Ok(db
        .prepare("SELECT workspace_id,state FROM workspace_agent_state")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

/// The workspace and state execution `id` last reported, while its record lasts.
fn reported_by(db: &Connection, id: &str) -> Result<Option<(String, AgentState)>> {
    Ok(db
        .query_row(
            "SELECT workspace_id,state FROM workspace_agent_state WHERE execution_id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?)
}

impl Manager {
    /// Keep the state execution `id` last reported in the recovery record it
    /// saved, before the state goes with the execution's record. A failure
    /// leaves the turn unknown, as in records saved before turns were kept.
    pub(crate) async fn keep_stopped_turn(&self, id: &str) {
        let query = id.to_owned();
        let kept = async {
            let reported = self.store.run(move |db| reported_by(db, &query)).await?;
            let Some((workspace, state)) = reported else {
                return Ok(());
            };
            recovery::save_turn(&self.paths, &workspace, id, state)
        }
        .await;
        if let Err(error) = kept {
            crate::daemon::log!("turn state of stopped execution {id} not kept: {error:#}");
        }
    }

    /// Record the state reported from `execution`, which clears it when its
    /// record goes, and the agent `session` it reports for resuming it.
    /// Repeating the current report writes nothing, since hooks repeat it
    /// after every tool call, and a repeated state keeps its start.
    pub async fn set_agent_state(
        &self,
        selector: &str,
        state: AgentState,
        execution: Option<String>,
        session: Option<String>,
    ) -> Result<AgentStatus> {
        let id = self.workspace(selector).await?.id;
        if let Some(execution) = &execution {
            self.record_agent_turn(execution, state).await;
        }
        if let (Some(execution), Some(session)) = (&execution, &session) {
            ensure!(recovery::valid_session(session), "invalid agent session ID");
            recovery::save_session(&self.paths, &id, execution, session)?;
        }
        let now = i64::try_from(crate::time::unix_seconds())?;
        let (status, changed) = self
            .store
            .run(move |db| {
                let tx = db.transaction()?;
                let changed = tx.execute(
                    "INSERT INTO workspace_agent_state(workspace_id,state,execution_id,since)
                     VALUES (?1,?2,?3,?4)
                     ON CONFLICT(workspace_id) DO UPDATE SET
                         since=CASE WHEN state=excluded.state THEN since ELSE excluded.since END,
                         state=excluded.state,execution_id=excluded.execution_id
                     WHERE state IS NOT excluded.state
                         OR execution_id IS NOT excluded.execution_id",
                    params![id, state, execution, now],
                )?;
                super::turn_retry::follow_state(&tx, &id, state)?;
                let status = get(&tx, &id)?.expect("agent state was just recorded");
                tx.commit()?;
                Ok((status, changed > 0))
            })
            .await?;
        if changed {
            self.agent_wake_changed.send_modify(|count| *count += 1);
        }
        Ok(status)
    }

    /// Record the turn `failure` ended as reported from `execution`, which
    /// leaves the agent idle and may retry the turn. The retry is scheduled
    /// first, since the agent going idle finishes a delivered one.
    pub async fn report_turn_failure(
        &self,
        selector: &str,
        failure: TurnFailure,
        execution: Option<String>,
        session: Option<String>,
    ) -> Result<AgentStatus> {
        if let Some(execution) = &execution {
            self.retry_failed_turn(selector, failure, execution).await;
        }
        self.set_agent_state(selector, AgentState::Idle, execution, session)
            .await
    }
}

#[cfg(test)]
mod tests;
