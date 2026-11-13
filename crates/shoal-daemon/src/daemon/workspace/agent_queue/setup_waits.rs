//! The launches waiting for the setup of a workspace whose setup failed:
//! returning a start whose preparation left none waiting to the queue,
//! finding the one waiting for a workspace's, and changing the permissions
//! its plan starts its agent with before a retried setup lets it start.
use crate::client::{queue::QueueEntryId, space_ids::SpaceId};
use std::collections::HashSet;

use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, Transaction, params};

use super::blockers;
use super::store::{ENTRY_COLUMNS, entry};
use crate::{
    config::permissions::PermissionsConfig,
    daemon::{log, store, workspace::Manager},
    model::QueueEntry,
    start_plan::AddPlan,
    state::{QueueKind, QueueState, WorkspaceState},
};

/// The waiting launches that open the workspace `?1`, which wait for its
/// failed setup, as `shoal restore` finds them among the queue's entries.
const AWAITING_SETUP: &str =
    "FROM agent_queue WHERE kind=?2 AND state=?3 AND opens_workspace_id=?1";

/// A starting launch in a workspace whose setup failed.
struct FailedStart {
    id: QueueEntryId,
    workspace: SpaceId,
    branch: Option<String>,
    plan: String,
}

impl Manager {
    /// Return to the queue each starting launch of a workspace whose setup
    /// failed once no operation works there, so no preparation leaves its
    /// agent waiting in its place, such as when the preparation's worker
    /// ended without doing so. Requires the queue gate.
    pub(super) async fn return_failed_starts(&self) -> Result<()> {
        let preparing = self.operations.working_in();
        let returned = self
            .store
            .write(move |tx| return_failed_starts(tx, &preparing))
            .await?;
        for (entry, workspace) in returned {
            log!("returned {entry} to the agent queue to wait for the setup of {workspace}");
        }
        Ok(())
    }

    /// The launch waiting for the setup of `workspace`, if one does.
    pub(crate) async fn launch_awaiting_setup(
        &self,
        workspace: &SpaceId,
    ) -> Result<Option<QueueEntry>> {
        let workspace = workspace.to_owned();
        self.store
            .run(move |db| {
                let query = format!("SELECT {ENTRY_COLUMNS} {AWAITING_SETUP}");
                let selection = params![workspace, QueueKind::Launch, QueueState::Queued];
                Ok(db.query_row(&query, selection, entry).optional()?)
            })
            .await
    }

    /// Give each launch `given` names the permissions beside it, in its
    /// plan, which its agent starts under, and in the entry the queue
    /// shows, refusing them all when one no longer waits to open its
    /// workspace.
    pub(crate) async fn give_launches_awaiting_setup(
        &self,
        given: Vec<(QueueEntryId, PermissionsConfig)>,
    ) -> Result<()> {
        let given = (given.into_iter())
            .map(|(id, given)| {
                let column = store::narrowing_column(&given)?;
                Ok((id, column, serde_json::to_string(&given)?))
            })
            .collect::<Result<Vec<_>>>()?;
        self.store
            .write(move |tx| {
                for (id, column, planned) in given {
                    let updated = tx.execute(
                        "UPDATE agent_queue SET narrowing=?2,plan=json_set(plan,'$.narrowing',json(?3))
                         WHERE id=?1 AND state=?4 AND json_valid(plan)
                            AND opens_workspace_id IS NOT NULL",
                        params![id, column, planned, QueueState::Queued],
                    )?;
                    ensure!(updated == 1, "{id} no longer waits to open its workspace");
                }
                Ok(())
            })
            .await
    }
}

/// Return each starting launch of a workspace outside `preparing` whose
/// setup failed to the queue, returning the new entries with their
/// workspaces.
fn return_failed_starts(
    tx: &Transaction<'_>,
    preparing: &HashSet<SpaceId>,
) -> Result<Vec<(QueueEntryId, SpaceId)>> {
    let mut returned = Vec::new();
    for start in failed_starts(tx)? {
        if !preparing.contains(&start.workspace) {
            returned.extend(return_start(tx, start)?);
        }
    }
    Ok(returned)
}

/// The starting launches in workspaces whose setup failed.
fn failed_starts(tx: &Transaction<'_>) -> Result<Vec<FailedStart>> {
    let selection = params![
        QueueKind::Launch,
        QueueState::Starting,
        WorkspaceState::Failed
    ];
    Ok(tx
        .prepare(
            "SELECT q.id,w.id,w.branch,q.plan FROM agent_queue q
             JOIN workspaces w ON w.id=COALESCE(q.space_id,q.opens_workspace_id)
             WHERE q.kind=?1 AND q.state=?2 AND w.state=?3 AND NOT w.setup_finished",
        )?
        .query_map(selection, |row| {
            Ok(FailedStart {
                id: row.get(0)?,
                workspace: row.get(1)?,
                branch: row.get(2)?,
                plan: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// Replace `start` with a new entry, which no hold on the old one keeps
/// from admission, waiting for a retried setup to open its workspace,
/// returning it with its workspace; one that cannot open it ends.
fn return_start(
    tx: &Transaction<'_>,
    start: FailedStart,
) -> Result<Option<(QueueEntryId, SpaceId)>> {
    let waiting = match start.reopening_plan() {
        Some(plan) => {
            let id = QueueEntryId::generate();
            tx.execute(
                "INSERT INTO agent_queue(id,kind,repository_id,name,agent,issue_title,plan,env,state,created_at,opens_workspace_id,mode,narrowing,scope_space_id,priority)
                 SELECT ?2,kind,repository_id,name,agent,issue_title,?3,env,?4,created_at,?5,mode,narrowing,scope_space_id,priority
                 FROM agent_queue WHERE id=?1",
                params![start.id, id, plan, QueueState::Queued, start.workspace],
            )?;
            blockers::wait_for_setup(tx, &id, &start.workspace)?;
            Some((id, start.workspace))
        }
        None => None,
    };
    tx.execute("DELETE FROM agent_queue WHERE id=?1", [&start.id])?;
    Ok(waiting)
}

impl FailedStart {
    /// Its plan opening the workspace instead of creating one, or `None`
    /// when it cannot, as for a workspace without a branch.
    fn reopening_plan(&self) -> Option<String> {
        let mut plan: AddPlan = serde_json::from_str(&self.plan).ok()?;
        plan.reopen(&self.workspace, self.branch.clone()?);
        serde_json::to_string(&plan).ok()
    }
}

#[cfg(test)]
mod tests;
