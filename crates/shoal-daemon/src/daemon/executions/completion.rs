//! Persist verified execution completion and setup readiness in one transaction.
use anyhow::Result;
use rusqlite::{OptionalExtension, Transaction, params};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;

use super::{ExecutionKind, Manager};
use crate::{
    daemon::approvals::{ApprovalRequest, release_decided_by},
    state::{ExecutionState, WorkspaceState},
};

/// What a verified completion recorded: the demo the execution ran, the
/// requests it gave back to the user, and the space whose agent it ran.
pub(super) struct Completed {
    pub demo: Option<String>,
    pub released: Vec<ApprovalRequest>,
    pub agent_space: Option<SpaceId>,
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
                let (demo, released, agent_space) = if complete {
                    let demo =
                        super::super::sessions::pending::record_demo(tx, &record_id, &hosts)?;
                    let released = release_attended_decisions(tx, &record_id)?;
                    (demo, released, record_agent_end(tx, &record_id)?)
                } else {
                    (None, Vec::new(), None)
                };
                record_execution_end(tx, &record_id, complete)?;
                Ok(Completed {
                    demo,
                    released,
                    agent_space,
                })
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
        let ended = id.to_owned();
        let hosts: Vec<_> = self.session_hosts.iter().map(|host| host.name()).collect();
        let (demo, agent_space, viewer) = self
            .store
            .write(move |tx| {
                let demo = super::super::sessions::pending::record_demo(tx, &ended, &hosts)?;
                let agent_space = record_agent_end(tx, &ended)?;
                // Read where its record goes, so a viewer recorded before
                // is stopped here and one recorded later cannot be.
                let viewer = crate::daemon::demos::viewers::recorded(tx, &ended)?;
                record_execution_end(tx, &ended, true)?;
                Ok((demo, agent_space, viewer))
            })
            .await?;
        if let Some(space) = &agent_space {
            self.agent_settles.request(space);
        }
        // A demo's viewer goes with it.
        self.viewers.end(id);
        if let Some(viewer) = viewer {
            crate::daemon::demos::viewers::stop_recorded(&viewer.process).await;
        }
        Ok(demo)
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
                AND EXISTS (SELECT 1 FROM spaces
                    WHERE spaces.id=executions.space_id AND spaces.kind='chat')",
            [id],
            |row| row.get(0),
        )
        .optional()?;
    match space {
        Some(space) => release_decided_by(tx, &space),
        None => Ok(Vec::new()),
    }
}

/// Record the end of agent execution `id` for its space, returning the
/// space; none for another execution.
fn record_agent_end(tx: &Transaction<'_>, id: &ExecutionId) -> Result<Option<SpaceId>> {
    let space = agent_execution_space(tx, id)?;
    if let Some(space) = &space {
        crate::daemon::links::issue::agent_ended(tx, space)?;
    }
    Ok(space)
}

/// The space whose agent execution `id` runs; none for another execution.
pub(crate) fn agent_execution_space(
    tx: &Transaction<'_>,
    id: &ExecutionId,
) -> Result<Option<SpaceId>> {
    Ok(tx
        .query_row(
            "SELECT space_id FROM executions WHERE id=?1 AND agent IS NOT NULL",
            [id],
            |row| row.get(0),
        )
        .optional()?)
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
