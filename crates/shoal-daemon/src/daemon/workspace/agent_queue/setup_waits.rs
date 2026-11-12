//! The launches waiting for the setup of a workspace whose setup failed:
//! finding the one waiting for a workspace's, and changing the permissions
//! it starts with before a retried setup lets it start.
use crate::client::{queue::QueueEntryId, space_ids::SpaceId};

use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, params};

use super::store::{ENTRY_COLUMNS, entry};
use crate::{
    config::permissions::PermissionsConfig,
    daemon::{store, workspace::Manager},
    model::QueueEntry,
    state::{BlockerKind, QueueKind, QueueState},
};

/// The waiting launches a failed setup of the workspace `?1` holds back.
const AWAITING_SETUP: &str = "FROM agent_queue WHERE kind=?2 AND state=?3 AND id IN
    (SELECT entry_id FROM agent_queue_blockers WHERE kind=?4 AND target=?1)";

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
                let kinds = (QueueKind::Launch, QueueState::Queued, BlockerKind::Setup);
                let selection = params![workspace, kinds.0, kinds.1, kinds.2];
                Ok(db.query_row(&query, selection, entry).optional()?)
            })
            .await
    }

    /// Give each launch `given` names the permissions beside it, refusing
    /// them all when one no longer waits for a setup.
    pub(crate) async fn give_launches_awaiting_setup(
        &self,
        given: Vec<(QueueEntryId, PermissionsConfig)>,
    ) -> Result<()> {
        let given = (given.into_iter())
            .map(|(id, given)| Ok((id, store::narrowing_column(&given)?)))
            .collect::<Result<Vec<_>>>()?;
        self.store
            .write(move |tx| {
                for (id, narrowing) in given {
                    let updated = tx.execute(
                        "UPDATE agent_queue SET narrowing=?2 WHERE id=?1 AND state=?3 AND id IN
                            (SELECT entry_id FROM agent_queue_blockers WHERE kind=?4)",
                        params![id, narrowing, QueueState::Queued, BlockerKind::Setup],
                    )?;
                    ensure!(updated == 1, "{id} no longer waits for a setup");
                }
                Ok(())
            })
            .await
    }
}
