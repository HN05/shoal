//! The walks of the tree of spaces in `space_parents`, and the reads of it
//! that more than one subsystem shares. Every subsystem that follows the
//! tree builds its queries on these walks.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::model::{RefPart, SpaceKind, SpaceRef};

/// The workspaces whose agent the daemon tracks. An agent counts whether it
/// reports turn states from hooks or not, as custom and Happy agents do not.
#[derive(Debug, Clone, Default)]
pub struct TrackedAgents(Vec<SpaceId>);

impl TrackedAgents {
    pub fn of(agents: &[SpaceId]) -> Self {
        Self(agents.to_vec())
    }

    /// The JSON array of IDs that [`runs_agent`] reads as `?2`.
    pub fn parameter(&self) -> String {
        serde_json::Value::from(self.0.iter().map(SpaceId::as_str).collect::<Vec<_>>()).to_string()
    }
}

/// The one walk up: a recursive query `up(id, depth)` of the space `?1` at
/// depth 0 and the spaces above it, nearest first, going on above a space
/// only while `climb`, an SQL condition on `up.id`, holds.
pub fn up(climb: &str) -> String {
    format!(
        "WITH RECURSIVE up(id, depth) AS (
            SELECT ?1, 0
            UNION ALL SELECT space_parents.parent_id, up.depth + 1 FROM up
                JOIN space_parents ON space_parents.space_id=up.id
                WHERE {climb}
        )"
    )
}

/// The one walk down: a recursive query `down(id, depth, path)` of the
/// space `?1` at depth 0 and the spaces below it, each with the IDs from
/// `?1` down to it joined by commas, entering a space below only while
/// `descend`, an SQL condition on its `space_parents.space_id`, holds.
pub fn down(descend: &str) -> String {
    format!(
        "WITH RECURSIVE down(id, depth, path) AS (
            SELECT ?1, 0, ?1
            UNION ALL SELECT space_parents.space_id, down.depth + 1,
                    down.path || ',' || space_parents.space_id
                FROM down JOIN space_parents ON space_parents.parent_id=down.id
                WHERE {descend}
        )"
    )
}

/// SQL true when the space in `column` has an agent: one that runs, an
/// agent reporting its turn state, or a queued restore, whose agent
/// receives what arrived once it starts.
pub fn has_agent(column: &str) -> String {
    format!(
        "({} OR EXISTS (SELECT 1 FROM space_agent_state WHERE space_agent_state.space_id={column})
            OR EXISTS (SELECT 1 FROM agent_queue
                WHERE agent_queue.space_id={column} AND agent_queue.kind='{}'))",
        runs_agent(column),
        shoal_protocol::state::QueueKind::Restore,
    )
}

/// SQL true when the agent of the space in `column` runs: one among the
/// tracked agents bound as parameter `?2`, or that of a chat that became
/// the workspace until it is handed over to it.
pub fn runs_agent(column: &str) -> String {
    format!(
        "({column} IN (SELECT value FROM json_each(?2))
            OR EXISTS (SELECT 1 FROM space_conversions WHERE space_conversions.space_id={column}
                AND json_extract(space_conversions.record,'$.to')='workspace'))"
    )
}

/// The first space above the space `id` that this machine does not have,
/// as when its parent runs on another joined machine.
pub fn first_absent_above(db: &Connection, id: &SpaceId) -> Result<Option<SpaceId>> {
    Ok(db
        .query_row(
            &format!(
                "{} SELECT id FROM up WHERE depth > 0
                    AND NOT EXISTS (SELECT 1 FROM spaces WHERE spaces.id=up.id)
                    ORDER BY depth LIMIT 1",
                up("true")
            ),
            [id],
            |row| row.get(0),
        )
        .optional()?)
}

/// `id` and every space below it, `id` first.
pub fn subtree(db: &Connection, id: &SpaceId) -> Result<Vec<SpaceId>> {
    Ok(db
        .prepare(&format!(
            "{} SELECT id FROM down ORDER BY depth, id",
            down("true")
        ))?
        .query_map([id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// The spaces above the space `id` that have an agent, nearest first.
pub fn spaces_above(db: &Connection, id: &SpaceId, agents: &TrackedAgents) -> Result<Vec<SpaceId>> {
    Ok(db
        .prepare(&format!(
            "{} SELECT id FROM up WHERE depth > 0 AND {} ORDER BY depth",
            up("true"),
            has_agent("up.id")
        ))?
        .query_map(params![id, agents.parameter()], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Where an event a space passes up goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Receiver {
    /// To the agent of this machine's space.
    Here(SpaceId),
    /// To the machine of the space, the first above that this machine has
    /// not, where the tree goes on.
    Elsewhere(SpaceId),
}

/// The coordinator of the space `id`: the nearest space above it with an
/// agent.
pub fn coordinator_of(
    db: &Connection,
    id: &SpaceId,
    agents: &TrackedAgents,
) -> Result<Option<SpaceId>> {
    Ok(spaces_above(db, id, agents)?.into_iter().next())
}

/// A space's ID, name, kind and branch, as the first four columns hold them.
pub fn space_ref(row: &rusqlite::Row<'_>) -> rusqlite::Result<SpaceRef> {
    Ok(SpaceRef {
        id: row.get(0)?,
        name: row.get(1)?,
        part: match row.get(2)? {
            SpaceKind::Workspace => RefPart::Workspace {
                branch: row.get(3)?,
            },
            SpaceKind::Chat => RefPart::Chat,
        },
    })
}

/// The spaces above the space `id`, nearest first.
pub fn ancestors(db: &Connection, id: &SpaceId) -> Result<Vec<SpaceRef>> {
    Ok(db
        .prepare(&format!(
            "{} SELECT spaces.id,spaces.name,spaces.kind,workspaces.branch FROM up
                JOIN spaces USING(id) LEFT JOIN workspaces ON workspaces.id=spaces.id
                WHERE depth > 0 ORDER BY depth",
            up("true")
        ))?
        .query_map([id], space_ref)?
        .collect::<rusqlite::Result<_>>()?)
}
