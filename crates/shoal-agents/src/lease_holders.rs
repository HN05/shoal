//! The agents that hold leases, as leases read them: a space's agent keeps
//! its leases from its first acquire until it ends for good, through its
//! sleeps, restores and restarts, and which agents sleep, since when.
use std::collections::{HashMap, HashSet};

use anyhow::Result;
use rusqlite::{Connection, params};
use shoal_ids::space_ids::SpaceId;

use crate::Agents;
use crate::state::{QueueKind, QueueState};
use crate::time::UnixTime;

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

    /// The spaces whose agent is asleep, each with when its harness
    /// stopped: none of its processes runs and its restore waits in the
    /// agent queue, which it entered as the agent stopped and leaves once
    /// it runs again, whatever wakes it meanwhile.
    pub async fn asleep_agents(&self) -> Result<HashMap<SpaceId, UnixTime>> {
        self.store.read(|db| asleep_agents(db)).await
    }
}

/// [`Agents::asleep_agents`] as the store records them.
fn asleep_agents(db: &Connection) -> Result<HashMap<SpaceId, UnixTime>> {
    Ok(db
        .prepare(
            "SELECT q.space_id,MIN(q.created_at) FROM agent_queue q
             WHERE q.kind=?1 AND q.state=?2 AND q.space_id IS NOT NULL
                AND NOT EXISTS (SELECT 1 FROM executions e
                    WHERE e.space_id=q.space_id AND e.agent IS NOT NULL
                        AND (e.child IS NOT NULL OR e.group_id IS NOT NULL))
             GROUP BY q.space_id",
        )?
        .query_map(params![QueueKind::Restore, QueueState::Queued], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?)
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
