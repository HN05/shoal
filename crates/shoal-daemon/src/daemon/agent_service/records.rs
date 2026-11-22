//! The records of other subsystems the agents read and write in their own
//! transactions.
use anyhow::Result;
use rusqlite::{Connection, Transaction};
use shoal_agents::daemon::Records;
use shoal_ids::{execution_ids::ExecutionId, space_ids::SpaceId};

use shoal_links::wait::followed::watches_open_pr;

use crate::daemon::{
    agent_queue, agents, changes,
    tree::{self, TrackedAgents},
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
}
