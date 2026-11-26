//! The launches waiting for the setup of a workspace.
use shoal_ids::{queue_entry_ids::QueueEntryId, space_ids::SpaceId};
use std::collections::HashSet;

use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, Transaction, params};

use super::blockers;
use super::store::{ENTRY_COLUMNS, copy_launch_waiting, entry};
use crate::Agents;
use crate::config::permissions::PermissionsConfig;
use crate::daemon::{Daemon, Records};
use crate::log;
use crate::model::QueueEntry;
use crate::state::QueueKind;
use crate::state::QueueState;
use crate::state::WorkspaceState;
use shoal_store;

/// The waiting launches that open the workspace `?1`, which wait for its
/// failed setup, as `shoal resume` finds them among the queue's entries.
const AWAITING_SETUP: &str =
    "FROM agent_queue WHERE kind=?2 AND state=?3 AND opens_workspace_id=?1";

/// A starting launch in a workspace whose setup failed.
struct FailedStart {
    id: QueueEntryId,
    workspace: SpaceId,
}

impl Agents {
    /// Start at once each launch waiting to open `workspace` that its start
    /// asked to, now that nothing blocks it, unless its caller holds it;
    /// one that cannot start stays for the queue to admit.
    pub async fn start_released_at_once(
        &self,
        daemon: &dyn Daemon,
        workspace: &SpaceId,
    ) -> Result<()> {
        let _gate = self.agent_queue.lock_admission().await;
        let workspace = workspace.to_owned();
        let released = (self.store)
            .run(move |db| released_at_once(db, &workspace))
            .await?;
        // A restore that runs the agent itself holds its entry meanwhile.
        let held = self.agent_queue.held();
        for id in released.into_iter().filter(|id| !held.contains(id)) {
            if let Err(error) = self.start_waiting_now(daemon, &id).await {
                log!("{id} did not start at once after its setup: {error:#}");
            }
        }
        Ok(())
    }

    /// Start the waiting entry `id` now, regardless of the limits. Requires
    /// the queue gate.
    async fn start_waiting_now(&self, daemon: &dyn Daemon, id: &QueueEntryId) -> Result<()> {
        let entry = self.waiting_entry(daemon, id.as_str(), None, "").await?;
        self.start_waiting(daemon, entry, false).await.map(drop)
    }

    /// Return to the queue each starting launch of a workspace whose setup
    /// failed once no operation works there, so no preparation leaves its
    /// agent waiting in its place, such as when the preparation's worker
    /// ended without doing so. Requires the queue gate.
    pub async fn return_failed_starts(&self) -> Result<()> {
        let (preparing, records) = (self.operations.working_in(), self.records.clone());
        let returned = self
            .store
            .write(move |tx| return_failed_starts(tx, &*records, &preparing))
            .await?;
        for (entry, workspace) in returned {
            log!("returned {entry} to the agent queue to wait for the setup of {workspace}");
        }
        Ok(())
    }

    /// The launch waiting for the setup of `workspace`, if one does.
    pub async fn launch_awaiting_setup(&self, workspace: &SpaceId) -> Result<Option<QueueEntry>> {
        let workspace = workspace.to_owned();
        self.store
            .run(move |db| {
                let query = format!("SELECT {ENTRY_COLUMNS} {AWAITING_SETUP}");
                let selection = params![workspace, QueueKind::Launch, QueueState::Queued];
                Ok(db.query_row(&query, selection, entry).optional()?)
            })
            .await
    }

    /// Give the permissions beside it to each launch `waiting` names and each
    /// failed launch `failed` names, in its plan and in the entry the queue
    /// shows, in one transaction. Refuses them all when one no longer waits to
    /// open its workspace or to be restored.
    pub async fn give_launches(
        &self,
        waiting: Vec<(QueueEntryId, PermissionsConfig)>,
        failed: Vec<(QueueEntryId, PermissionsConfig)>,
    ) -> Result<()> {
        let (waiting, failed) = (given_columns(waiting)?, given_columns(failed)?);
        self.store
            .write(move |tx| {
                for (id, column, planned) in waiting {
                    let updated = tx.execute(
                        "UPDATE agent_queue SET narrowing=?2,plan=json_set(plan,'$.launch.narrowing',json(?3))
                         WHERE id=?1 AND state=?4 AND json_valid(plan)
                            AND opens_workspace_id IS NOT NULL",
                        params![id, column, planned, QueueState::Queued],
                    )?;
                    ensure!(updated == 1, "{id} no longer waits to open its workspace");
                }
                for (id, column, planned) in failed {
                    super::failures::give(tx, &id, &column, &planned)?;
                }
                Ok(())
            })
            .await
    }
}

/// The permissions `given` each launch, as its entry's column and its
/// plan's JSON record them.
type GivenColumns = Vec<(QueueEntryId, Option<String>, String)>;

/// [`GivenColumns`] of the permissions `given` each launch.
fn given_columns(given: Vec<(QueueEntryId, PermissionsConfig)>) -> Result<GivenColumns> {
    (given.into_iter())
        .map(|(id, given)| {
            let column = shoal_store::narrowing_column(&given)?;
            Ok((id, column, serde_json::to_string(&given)?))
        })
        .collect()
}

/// The launches waiting to open `workspace` that their start asked to
/// start at once and that nothing blocks any longer.
fn released_at_once(db: &rusqlite::Connection, workspace: &SpaceId) -> Result<Vec<QueueEntryId>> {
    let query = format!(
        "SELECT id {AWAITING_SETUP} AND starts_at_once
            AND NOT EXISTS (SELECT 1 FROM agent_queue_blockers b WHERE b.entry_id=agent_queue.id)"
    );
    let selection = params![workspace, QueueKind::Launch, QueueState::Queued];
    Ok(db
        .prepare(&query)?
        .query_map(selection, |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Return each starting launch of a workspace outside `preparing` whose
/// setup failed to the queue, returning each new entry with its workspace.
fn return_failed_starts(
    tx: &Transaction<'_>,
    records: &dyn Records,
    preparing: &HashSet<SpaceId>,
) -> Result<Vec<(QueueEntryId, SpaceId)>> {
    let mut returned = Vec::new();
    for start in failed_starts(tx, records)? {
        if !preparing.contains(&start.workspace) {
            returned.push(return_start(tx, start)?);
        }
    }
    Ok(returned)
}

/// The starting launches in workspaces whose setup failed.
fn failed_starts(tx: &Transaction<'_>, records: &dyn Records) -> Result<Vec<FailedStart>> {
    let failed: HashSet<SpaceId> = (records.spaces(tx)?.into_iter())
        .filter(|space| space.state == Some(WorkspaceState::Failed) && !space.setup_finished)
        .map(|space| space.id)
        .collect();
    let starting = tx
        .prepare(
            "SELECT id,COALESCE(space_id,opens_workspace_id) FROM agent_queue
             WHERE kind=?1 AND state=?2",
        )?
        .query_map(params![QueueKind::Launch, QueueState::Starting], |row| {
            Ok((row.get(0)?, row.get::<_, Option<SpaceId>>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((starting.into_iter())
        .filter_map(|(id, workspace)| {
            let workspace = workspace.filter(|workspace| failed.contains(workspace))?;
            Some(FailedStart { id, workspace })
        })
        .collect())
}

/// Replace `start` with a new entry, which no hold on the old one keeps
/// from admission, waiting for a retried setup to open its workspace,
/// returning the new entry with the workspace.
fn return_start(tx: &Transaction<'_>, start: FailedStart) -> Result<(QueueEntryId, SpaceId)> {
    let id = QueueEntryId::generate();
    copy_launch_waiting(tx, &start.id, &id, &start.workspace)?;
    blockers::wait_for_setup(tx, &id, &start.workspace)?;
    tx.execute("DELETE FROM agent_queue WHERE id=?1", [&start.id])?;
    Ok((id, start.workspace))
}
