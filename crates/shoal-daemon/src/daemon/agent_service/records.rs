//! The records of other subsystems the agents read and write in their own
//! transactions.
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use shoal_agents::daemon::Records;
use shoal_ids::{execution_ids::ExecutionId, repository_ids::RepositoryId, space_ids::SpaceId};

use shoal_links::wait::followed::watches_open_pr;

use crate::{
    daemon::{
        agent_queue, agents, changes, store,
        tree::{self, TrackedAgents},
    },
    state::WorkspaceState,
};

pub(crate) struct AgentRecords;

impl Records for AgentRecords {
    fn end_commands(&self, tx: &Transaction<'_>, id: &SpaceId) -> Result<()> {
        changes::records::end_commands(tx, id)
    }

    fn stop_recorded(&self, db: &Connection, id: &ExecutionId) -> Result<bool> {
        agents::stop_recorded(db, id)
    }

    fn wake_asleep(&self, db: &Connection, id: &SpaceId) -> Result<()> {
        agent_queue::asleep::wake(db, id)
    }

    fn receives_open_pr_activity(
        &self,
        db: &Connection,
        id: &SpaceId,
        running: &[SpaceId],
    ) -> Result<bool> {
        let agents = TrackedAgents::of(running);
        let children = tree::delegated(db, id, &agents)?
            .into_iter()
            .map(|child| child.id);
        for space in std::iter::once(id.to_owned()).chain(children) {
            if watches_open_pr(db, &space)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn space_done(&self, db: &Connection, id: &SpaceId) -> Result<bool> {
        store::exists(db, "SELECT 1 FROM space_completion WHERE space_id=?1", [id])
    }

    fn space_is_workspace(&self, db: &Connection, id: &SpaceId) -> Result<bool> {
        store::exists(
            db,
            "SELECT 1 FROM spaces WHERE id=?1 AND kind='workspace'",
            [id],
        )
    }

    fn ensure_takes_alerts(&self, db: &Connection, id: &SpaceId) -> Result<()> {
        let present: Option<bool> = db
            .query_row(
                "SELECT state IS NOT ?2 FROM spaces LEFT JOIN workspaces USING(id) WHERE id=?1",
                params![id, WorkspaceState::Removing],
                |row| row.get(0),
            )
            .optional()?;
        ensure!(present.is_some(), "no space {id}");
        ensure!(present == Some(true), "the workspace is being removed");
        Ok(())
    }

    fn repository_registered(&self, db: &Connection, id: &RepositoryId) -> Result<bool> {
        store::exists(db, "SELECT 1 FROM repositories WHERE id=?1", [id])
    }
}
