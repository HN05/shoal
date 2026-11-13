//! The launches waiting for the setup of a workspace whose setup failed:
//! finding the one waiting for a workspace's, and changing the permissions
//! its plan starts its agent with before a retried setup lets it start.
use crate::client::{queue::QueueEntryId, space_ids::SpaceId};

use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, params};

use super::store::{ENTRY_COLUMNS, entry};
use crate::{
    config::permissions::PermissionsConfig,
    daemon::{store, workspace::Manager},
    model::QueueEntry,
    state::{QueueKind, QueueState},
};

/// The waiting launches that open the workspace `?1`, which wait for its
/// failed setup, as `shoal restore` finds them among the queue's entries.
const AWAITING_SETUP: &str =
    "FROM agent_queue WHERE kind=?2 AND state=?3 AND opens_workspace_id=?1";

impl Manager {
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
