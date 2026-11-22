//! The records of other subsystems the agents read and write in their own
//! transactions.
use anyhow::Result;
use rusqlite::{Connection, Transaction};
use shoal_agents::daemon::Records;
use shoal_ids::{execution_ids::ExecutionId, space_ids::SpaceId};

use crate::daemon::{agent_queue, agents, changes};

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
}
