//! Persist verified execution completion and setup readiness in one transaction.
use crate::client::execution_ids::ExecutionId;
use crate::client::space_ids::SpaceId;
use anyhow::Result;
use rusqlite::{OptionalExtension, Transaction, params};

use super::{ExecutionKind, Manager};
use crate::{
    daemon::approvals::{ApprovalRequest, release_decided_by},
    state::{ExecutionState, WorkspaceState},
};

/// What a verified completion recorded: the demo the execution ran, and
/// the requests it gave back to the user.
pub(super) struct Completed {
    pub demo: Option<String>,
    pub released: Vec<ApprovalRequest>,
}

impl Manager {
    pub(super) async fn record_execution_completion(
        &self,
        id: &ExecutionId,
        kind: ExecutionKind,
        exit_code: Option<i32>,
        issue: Option<String>,
    ) -> Result<Completed> {
        let complete = issue.is_none();
        let record_id = id.to_owned();
        let hosts: Vec<_> = self.session_hosts.iter().map(|host| host.name()).collect();
        self.store
            .write(move |tx| {
                if kind == ExecutionKind::Setup {
                    record_setup_result(tx, &record_id, exit_code, issue.as_deref())?;
                }
                let (demo, released) = if complete {
                    let demo =
                        super::super::sessions::pending::record_demo(tx, &record_id, &hosts)?;
                    (demo, release_attended_decisions(tx, &record_id)?)
                } else {
                    (None, Vec::new())
                };
                record_execution_end(tx, &record_id, complete)?;
                Ok(Completed { demo, released })
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
        "UPDATE workspaces SET state=?2,error=?3,setup_finished=?4 WHERE id=(SELECT space_id FROM executions WHERE id=?1) AND state=?5",
        params![id, state, error, error.is_none(), WorkspaceState::Preparing],
    )?;
    Ok(())
}

/// Fail the preparing workspace of setup execution `id`, which never ran.
fn record_setup_not_started(tx: &Transaction<'_>, id: &ExecutionId) -> Result<()> {
    tx.execute(
        "UPDATE workspaces SET state=?2,error=?3,setup_finished=0 WHERE id=(SELECT space_id FROM executions WHERE id=?1) AND state=?4",
        params![id, WorkspaceState::Failed, SETUP_NOT_STARTED, WorkspaceState::Preparing],
    )?;
    Ok(())
}

/// Why a workspace whose setup never ran is failed.
const SETUP_NOT_STARTED: &str =
    "setup did not start: its wrapper left before the daemon registered it; retry with shoal setup";

/// Give the user back the requests the space of execution `id` decides
/// when the user attends that space, a chat, whose one execution is its
/// agent: without it no one is there to decide them.
fn release_attended_decisions(
    tx: &Transaction<'_>,
    id: &ExecutionId,
) -> Result<Vec<ApprovalRequest>> {
    let space: Option<SpaceId> = tx
        .query_row(
            "SELECT space_id FROM executions WHERE id=?1
                AND EXISTS (SELECT 1 FROM chats WHERE chats.id=executions.space_id)",
            [id],
            |row| row.get(0),
        )
        .optional()?;
    match space {
        Some(space) => release_decided_by(tx, &space),
        None => Ok(Vec::new()),
    }
}

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
