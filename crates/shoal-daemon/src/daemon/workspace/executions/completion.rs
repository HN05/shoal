//! Persist verified execution completion and setup readiness in one transaction.
use crate::client::execution_ids::ExecutionId;
use anyhow::Result;
use rusqlite::{Transaction, params};

use super::{ExecutionKind, Manager};
use crate::state::{ExecutionState, WorkspaceState};

impl Manager {
    pub(super) async fn record_execution_completion(
        &self,
        id: &ExecutionId,
        kind: ExecutionKind,
        exit_code: Option<i32>,
        issue: Option<String>,
    ) -> Result<Option<String>> {
        let complete = issue.is_none();
        let record_id = id.to_owned();
        let hosts: Vec<_> = self.session_hosts.iter().map(|host| host.name()).collect();
        self.store
            .write(move |tx| {
                if kind == ExecutionKind::Setup {
                    record_setup_result(tx, &record_id, exit_code, issue.as_deref())?;
                }
                let demo = if complete {
                    super::super::sessions::pending::record_demo(tx, &record_id, &hosts)?
                } else {
                    None
                };
                record_execution_end(tx, &record_id, complete)?;
                Ok(demo)
            })
            .await
    }

    /// Forget execution `id` of `kind`, whose wrapper never received its
    /// plan, so its command never ran. A setup leaves its preparing workspace
    /// failed with that, rather than with a setup failure.
    pub(crate) async fn cancel_execution(
        &self,
        id: &ExecutionId,
        kind: ExecutionKind,
    ) -> Result<()> {
        let connections = self.executions.connections().await;
        let record_id = id.to_owned();
        self.store
            .write(move |tx| {
                if kind == ExecutionKind::Setup {
                    record_setup_not_started(tx, &record_id)?;
                }
                record_execution_end(tx, &record_id, true)
            })
            .await?;
        if connections.disconnect(id).await {
            self.agent_queue.wake();
        }
        Ok(())
    }

    pub(super) async fn delete_execution_record(&self, id: &ExecutionId) -> Result<Option<String>> {
        let id = id.to_owned();
        let hosts: Vec<_> = self.session_hosts.iter().map(|host| host.name()).collect();
        self.store
            .write(move |tx| {
                let demo = super::super::sessions::pending::record_demo(tx, &id, &hosts)?;
                record_execution_end(tx, &id, true)?;
                Ok(demo)
            })
            .await
    }
}

/// Persist setup readiness and its diagnostic only for the preparing workspace
/// that owns this execution; verification failure never readies it.
fn record_setup_result(
    tx: &Transaction<'_>,
    id: &ExecutionId,
    exit_code: Option<i32>,
    issue: Option<&str>,
) -> Result<()> {
    let complete = issue.is_none();
    let code = exit_code.unwrap_or(1);
    let error = (!(complete && code == 0)).then(|| {
        let reason = issue.map_or(String::new(), |issue| format!("; {issue}"));
        if code == 0 {
            format!("setup command exited successfully (exit 0), but process verification is incomplete{reason}; stop its processes with shoal stop, then retry or skip setup")
        } else {
            format!("setup failed (exit {code}, processes stopped: {complete}){reason}; retry with shoal setup")
        }
    });
    let state = if error.is_none() {
        WorkspaceState::Ready
    } else {
        WorkspaceState::Failed
    };
    tx.execute(
        "UPDATE workspaces SET state=?2,error=?3,setup_finished=?4 WHERE id=(SELECT workspace_id FROM executions WHERE id=?1) AND state=?5",
        params![id, state, error, error.is_none(), WorkspaceState::Preparing],
    )?;
    Ok(())
}

/// Fail the preparing workspace of setup execution `id`, which never ran.
fn record_setup_not_started(tx: &Transaction<'_>, id: &ExecutionId) -> Result<()> {
    tx.execute(
        "UPDATE workspaces SET state=?2,error=?3,setup_finished=0 WHERE id=(SELECT workspace_id FROM executions WHERE id=?1) AND state=?4",
        params![id, WorkspaceState::Failed, SETUP_NOT_STARTED, WorkspaceState::Preparing],
    )?;
    Ok(())
}

/// Why a workspace whose setup never ran is failed.
const SETUP_NOT_STARTED: &str =
    "setup did not start: its wrapper left before the daemon registered it; retry with shoal setup";

/// Retain unverified executions for recovery; only verified completion deletes
/// the record, together with any demo it owns.
fn record_execution_end(tx: &Transaction<'_>, id: &ExecutionId, complete: bool) -> Result<()> {
    if complete {
        tx.execute("DELETE FROM executions WHERE id=?1", [id])?;
    } else {
        tx.execute(
            "UPDATE executions SET state=?2 WHERE id=?1",
            params![id, ExecutionState::Unknown],
        )?;
    }
    Ok(())
}
