//! The turn state agents report from their hooks: a status signal for
//! integrations that never notifies, completes, or changes cleanup.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

use super::Manager;
use crate::{model::AgentStatus, state::AgentState};

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

impl Manager {
    /// Record the state reported from `execution`, which clears it when its
    /// record goes. Repeating the current report writes nothing, since hooks
    /// repeat it after every tool call, and a repeated state keeps its start.
    pub async fn set_agent_state(
        &self,
        selector: &str,
        state: AgentState,
        execution: Option<String>,
    ) -> Result<AgentStatus> {
        let id = self.workspace(selector).await?.id;
        let now = i64::try_from(crate::time::unix_seconds())?;
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                tx.execute(
                    "INSERT INTO workspace_agent_state(workspace_id,state,execution_id,since)
                     VALUES (?1,?2,?3,?4)
                     ON CONFLICT(workspace_id) DO UPDATE SET
                         since=CASE WHEN state=excluded.state THEN since ELSE excluded.since END,
                         state=excluded.state,execution_id=excluded.execution_id
                     WHERE state IS NOT excluded.state
                         OR execution_id IS NOT excluded.execution_id",
                    params![id, state, execution, now],
                )?;
                let status = get(&tx, &id)?.expect("agent state was just recorded");
                tx.commit()?;
                Ok(status)
            })
            .await
    }
}

#[cfg(test)]
mod tests;
