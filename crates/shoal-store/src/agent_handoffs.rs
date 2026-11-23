//! The handoffs of spaces' work from one agent tool to another: the spaces
//! record each as it happens, and the session report of the new tool's
//! agent completes it, which journals it.
use std::collections::HashMap;

use anyhow::Result;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use shoal_ids::{
    agent_session_ids::AgentSessionId, execution_ids::ExecutionId, space_ids::SpaceId,
};
use shoal_protocol::transcripts::HandedOverSession;

/// The two sides of a handoff, as its record keeps them.
#[derive(Serialize, Deserialize)]
struct Sides {
    from: HandedOverSession,
    to: HandedOverSession,
}

/// Record that the agent `to` names takes the work of the space `space_id`
/// over from the session `from`. An earlier handoff of the space whose
/// agent never reported a session goes: nothing continues from it.
pub fn record(
    tx: &Connection,
    space_id: &SpaceId,
    from: &HandedOverSession,
    to: &HandedOverSession,
) -> Result<()> {
    tx.execute(
        "DELETE FROM agent_handoffs WHERE space_id=?1 AND session IS NULL",
        [space_id],
    )?;
    let sides = Sides {
        from: from.clone(),
        to: to.clone(),
    };
    tx.execute(
        "INSERT INTO agent_handoffs(space_id,agent,record) VALUES (?1,?2,?3)",
        params![space_id, to.agent, serde_json::to_string(&sides)?],
    )?;
    Ok(())
}

/// Complete the handoff of the space `space_id` that waits for a session of
/// the agent `execution` runs, which reported `session`.
pub fn started(
    tx: &Connection,
    space_id: &SpaceId,
    execution: &ExecutionId,
    session: &AgentSessionId,
) -> Result<()> {
    tx.execute(
        "UPDATE agent_handoffs SET session=?3
         WHERE space_id=?1 AND session IS NULL
            AND agent=(SELECT agent FROM executions WHERE id=?2)",
        params![space_id, execution, session],
    )?;
    Ok(())
}

/// The session each session a handoff of the space `space_id` started
/// took over from, by the session it started.
pub fn continued_from(
    db: &Connection,
    space_id: &SpaceId,
) -> Result<HashMap<AgentSessionId, HandedOverSession>> {
    let rows: Vec<(AgentSessionId, String)> = db
        .prepare(
            "SELECT session,record FROM agent_handoffs
             WHERE space_id=?1 AND session IS NOT NULL ORDER BY id",
        )?
        .query_map([space_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    rows.into_iter()
        .map(|(session, record)| {
            let sides: Sides = serde_json::from_str(&record)?;
            Ok((session, sides.from))
        })
        .collect()
}

#[cfg(test)]
mod tests;
