//! What the daemon records of a space's agent, which settling the agent
//! account on the issues the space links reads.
use anyhow::Result;
use rusqlite::Connection;
use shoal_ids::space_ids::SpaceId;
use shoal_links::daemon::SpaceAgent;

use crate::{
    daemon::{agent_queue, manager::Manager},
    execution::recovery,
};

/// What the store records of a space's agent.
struct AgentRecord {
    /// Whether the space's work is done.
    done: bool,
    /// Whether it has an agent that runs, is queued, restarts or waits for
    /// a conversion to restart it.
    present: bool,
}

impl Manager {
    /// What the store records of the agent of the space `id`, which
    /// settling the agent account on its issues reads.
    pub(in crate::daemon::links) async fn issue_agent(&self, id: &SpaceId) -> Result<SpaceAgent> {
        let owned = id.to_owned();
        let record = self.store.read(move |tx| agent_record(tx, &owned)).await?;
        Ok(SpaceAgent {
            done: record.done,
            present: record.present || self.agent_sleeps(id)?,
        })
    }

    /// Whether the agent of the space `id` is asleep, stopped by Shoal with
    /// a restore requested that the agent queue has yet to take.
    fn agent_sleeps(&self, id: &SpaceId) -> Result<bool> {
        Ok(!recovery::restore_requests(&self.paths, id)?.is_empty())
    }
}

/// What the store records of the agent of the space `id`.
fn agent_record(db: &Connection, id: &SpaceId) -> Result<AgentRecord> {
    let (done, runs) = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM space_completion WHERE space_id=?1),
            EXISTS(SELECT 1 FROM executions WHERE space_id=?1 AND agent IS NOT NULL)
            OR EXISTS(SELECT 1 FROM space_conversions WHERE space_id=?1)",
        [id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok(AgentRecord {
        done,
        present: runs || agent_queue::store::queues_agent_of(db, id)?,
    })
}

#[cfg(test)]
mod tests;
