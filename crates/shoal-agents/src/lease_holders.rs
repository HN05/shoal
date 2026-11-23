//! The agents that hold leases, as leases read them: a space's agent keeps
//! its leases from its first acquire until it ends for good, through its
//! sleeps, restores and restarts.
use std::collections::HashSet;

use anyhow::Result;
use rusqlite::{Connection, params};
use shoal_ids::space_ids::SpaceId;

use crate::Agents;
use crate::state::QueueKind;

impl Agents {
    /// The spaces whose agent has not ended for good: one recorded as
    /// running or whose wrapper waits to restore it, one whose restore
    /// waits in the agent queue or starts, and one a restart starts in a
    /// new wrapper.
    pub async fn agent_spaces(&self) -> Result<HashSet<SpaceId>> {
        // The restarts first: one that ends meanwhile has queued its
        // restore, which the records read after hold.
        let mut spaces = self.renewing_workspaces().await;
        spaces.extend(self.store.read(|db| recorded_agent_spaces(db)).await?);
        Ok(spaces)
    }
}

/// The spaces with an agent execution or a queued restore.
fn recorded_agent_spaces(db: &Connection) -> Result<HashSet<SpaceId>> {
    Ok(db
        .prepare(
            "SELECT space_id FROM executions WHERE agent IS NOT NULL
             UNION SELECT space_id FROM agent_queue WHERE kind=?1 AND space_id IS NOT NULL",
        )?
        .query_map(params![QueueKind::Restore], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}
