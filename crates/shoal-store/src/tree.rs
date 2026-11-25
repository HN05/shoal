//! The one walk up and the one walk down the tree of spaces in
//! `space_parents`, which every subsystem that follows the tree takes in its
//! own transactions, with whether a space on the way has an agent: the
//! spaces above and below a space, its coordinator and the spaces that
//! decide its requests, and the order removals take spaces in.
use std::collections::{HashMap, HashSet};

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use shoal_ids::space_ids::SpaceId;

use crate::model::{RefPart, SpaceKind, SpaceRef};

/// Each space a removal of another takes first, as `space_id, parent_id`:
/// the spaces below it, and the chats in a workspace's worktree, which are
/// not below it but cannot outlive it.
pub const TAKEN_FIRST: &str = "(SELECT space_id,parent_id FROM space_parents
    UNION ALL SELECT id,workspace_id FROM chats WHERE workspace_id IS NOT NULL)";

/// The workspaces whose agent the daemon tracks. An agent counts whether it
/// reports turn states from hooks or not, as custom and Happy agents do not.
#[derive(Debug, Clone, Default)]
pub struct TrackedAgents(Vec<SpaceId>);

impl TrackedAgents {
    /// The spaces `agents` names as those whose agent the daemon tracks.
    pub fn of(agents: &[SpaceId]) -> Self {
        Self(agents.to_vec())
    }

    /// The parameter `has_agent` reads.
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

/// The recursive query `taken(id, depth)` of the space `?1` and each space
/// its removal takes before it. The parents a space may take keep it free
/// of cycles.
pub fn taken() -> String {
    format!(
        "WITH RECURSIVE taken(id, depth) AS (
            SELECT ?1, 0
            UNION ALL SELECT first.space_id, taken.depth + 1
                FROM taken JOIN {TAKEN_FIRST} AS first ON first.parent_id=taken.id
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
        crate::state::QueueKind::Restore,
    )
}

/// SQL true when the agent of the space in `column` runs: one among the
/// tracked agents bound as parameter `?2`, or that of a chat that became
/// the workspace until it is handed over to it.
fn runs_agent(column: &str) -> String {
    format!(
        "({column} IN (SELECT value FROM json_each(?2))
            OR EXISTS (SELECT 1 FROM space_conversions WHERE space_conversions.space_id={column}
                AND json_extract(space_conversions.record,'$.to')='workspace'))"
    )
}

/// Whether the agent of the space `id` runs, in any turn state.
pub fn agent_runs(db: &Connection, id: &SpaceId, agents: &TrackedAgents) -> Result<bool> {
    crate::exists(
        db,
        &format!("SELECT 1 WHERE {}", runs_agent("?1")),
        params![id, agents.parameter()],
    )
}

/// The ID of the space directly above the space `id`.
pub fn parent_id(db: &Connection, id: &SpaceId) -> Result<Option<SpaceId>> {
    Ok(db
        .prepare_cached("SELECT parent_id FROM space_parents WHERE space_id=?1")?
        .query_row([id], |row| row.get(0))
        .optional()?)
}

/// The space above each space that has one, which may run on another
/// machine, by the ID of the space below it.
pub fn parent_ids(db: &Connection) -> Result<HashMap<SpaceId, SpaceId>> {
    Ok(db
        .prepare("SELECT space_id, parent_id FROM space_parents")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

/// This machine's spaces whose parent it does not have, each with that
/// parent, which runs on another machine.
pub fn below_elsewhere(db: &Connection) -> Result<Vec<(SpaceId, SpaceId)>> {
    Ok(db
        .prepare(
            "SELECT space_id, parent_id FROM space_parents
                WHERE NOT EXISTS (SELECT 1 FROM spaces WHERE spaces.id=space_parents.parent_id)",
        )?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

/// This machine's spaces whose activity goes to a coordinator on another
/// machine, by the space above them there: each space without an agent
/// whose parent runs elsewhere, then those below it whose activity it
/// would receive, as [`delegated`] walks them.
pub fn delegated_elsewhere(
    db: &Connection,
    agents: &TrackedAgents,
) -> Result<Vec<(SpaceId, Vec<SpaceRef>)>> {
    let with_agents = with_agents(db, agents)?;
    let mut groups = Vec::new();
    for (space, above) in below_elsewhere(db)? {
        if with_agents.contains(&space) {
            continue;
        }
        let Some(own) = space_ref_of(db, &space)? else {
            continue;
        };
        let spaces = [own].into_iter().chain(delegated(db, &space, agents)?);
        groups.push((above, spaces.collect()));
    }
    Ok(groups)
}

/// The space `id`, if this machine has it.
fn space_ref_of(db: &Connection, id: &SpaceId) -> Result<Option<SpaceRef>> {
    Ok(db
        .query_row(
            "SELECT spaces.id,spaces.name,spaces.kind,workspaces.branch FROM spaces
                LEFT JOIN workspaces ON workspaces.id=spaces.id WHERE spaces.id=?1",
            [id],
            space_ref,
        )
        .optional()?)
}

/// The spaces with an agent among `agents`, as [`has_agent`] counts one.
pub fn with_agents(db: &Connection, agents: &TrackedAgents) -> Result<HashSet<SpaceId>> {
    Ok(db
        .prepare(&format!(
            "SELECT id FROM spaces WHERE {}",
            has_agent("spaces.id")
        ))?
        .query_map(params![Option::<String>::None, agents.parameter()], |row| {
            row.get(0)
        })?
        .collect::<rusqlite::Result<_>>()?)
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

/// A query of `columns` from the workspace whose ID is parameter 1 and the
/// workspaces below it, through spaces of either kind, or from every
/// workspace when the parameter is NULL, by name.
pub fn subtree_query(columns: &str) -> String {
    format!(
        "{} SELECT {columns} FROM spaces JOIN workspaces USING(id)
            WHERE ?1 IS NULL OR id IN (SELECT id FROM down) ORDER BY name",
        down("true")
    )
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

/// The spaces directly below the space `id`, by name.
pub fn children(db: &Connection, id: &SpaceId) -> Result<Vec<SpaceRef>> {
    Ok(db
        .prepare_cached(
            "SELECT spaces.id,spaces.name,spaces.kind,workspaces.branch FROM space_parents
                JOIN spaces ON spaces.id=space_parents.space_id
                LEFT JOIN workspaces ON workspaces.id=spaces.id
                WHERE space_parents.parent_id=?1 ORDER BY spaces.name",
        )?
        .query_map([id], space_ref)?
        .collect::<rusqlite::Result<_>>()?)
}

/// The space directly above the space `id`.
pub fn parent(db: &Connection, id: &SpaceId) -> Result<Option<SpaceRef>> {
    Ok(db
        .prepare_cached(
            "SELECT spaces.id,spaces.name,spaces.kind,workspaces.branch FROM space_parents
                JOIN spaces ON spaces.id=space_parents.parent_id
                LEFT JOIN workspaces ON workspaces.id=spaces.id
                WHERE space_parents.space_id=?1",
        )?
        .query_row([id], space_ref)
        .optional()?)
}

/// The spaces below `id` whose activity its agent receives.
pub fn delegated(db: &Connection, id: &SpaceId, agents: &TrackedAgents) -> Result<Vec<SpaceRef>> {
    Ok(db
        .prepare(&format!(
            "{} SELECT spaces.id,spaces.name,spaces.kind,workspaces.branch FROM down
                JOIN spaces USING(id) LEFT JOIN workspaces ON workspaces.id=spaces.id
                WHERE depth > 0 ORDER BY depth, name",
            down(&format!("NOT {}", has_agent("space_parents.space_id")))
        ))?
        .query_map(params![id, agents.parameter()], space_ref)?
        .collect::<rusqlite::Result<_>>()?)
}

/// The space `id` and those below it whose activity its agent receives,
/// each as the path of spaces down to it from the topmost space that
/// receives `id`'s own activity: `id` itself when it has an agent, otherwise
/// the nearest ancestor that has one, or the root.
pub fn receiving_paths(
    db: &Connection,
    id: &SpaceId,
    agents: &TrackedAgents,
) -> Result<Vec<Vec<SpaceId>>> {
    let receivers = receiving_ancestors(db, id, agents)?;
    Ok(delegation_paths(db, id, agents)?
        .into_iter()
        .map(|path| receivers.iter().cloned().chain(path).collect())
        .collect())
}

/// The ancestors that receive `id`'s activity, topmost first: each parent
/// while the space below it has no agent of its own.
fn receiving_ancestors(
    db: &Connection,
    id: &SpaceId,
    agents: &TrackedAgents,
) -> Result<Vec<SpaceId>> {
    Ok(db
        .prepare(&format!(
            "{} SELECT id FROM up WHERE depth > 0 ORDER BY depth DESC",
            up(&format!("NOT {}", has_agent("up.id")))
        ))?
        .query_map(params![id, agents.parameter()], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// The space `id` and those below it whose activity its agent receives,
/// each as the path of spaces from `id` down to it.
fn delegation_paths(
    db: &Connection,
    id: &SpaceId,
    agents: &TrackedAgents,
) -> Result<Vec<Vec<SpaceId>>> {
    db.prepare(&format!(
        "{} SELECT path FROM down ORDER BY depth, id",
        down(&format!("NOT {}", has_agent("space_parents.space_id")))
    ))?
    .query_map(params![id, agents.parameter()], |row| {
        row.get::<_, String>(0)
    })?
    .map(|path| {
        Ok(path?
            .split(',')
            .map(|id| SpaceId::from(id.to_owned()))
            .collect())
    })
    .collect()
}

/// The length of the longest chain of spaces whose removals take the
/// space `id` first, each the next one's, as [`TAKEN_FIRST`] links them.
pub fn removal_depth(db: &Connection, id: &SpaceId) -> Result<u32> {
    let sql = format!(
        "WITH RECURSIVE taking(id, depth) AS (
            SELECT ?1, 0
            UNION ALL SELECT first.parent_id, taking.depth + 1
                FROM taking JOIN {TAKEN_FIRST} AS first ON first.space_id=taking.id
        ) SELECT MAX(depth) FROM taking"
    );
    Ok(db.query_row(&sql, [id], |row| row.get(0))?)
}

/// The spaces above the space `id` with an agent, which may decide what it
/// passes up, nearest first.
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

/// What receives an event passed up to the space `id` from below: the
/// nearest space from `id` up with an agent, or the first one above on
/// another machine.
pub fn receiver_from(
    db: &Connection,
    id: &SpaceId,
    agents: &TrackedAgents,
) -> Result<Option<Receiver>> {
    let here = db
        .query_row(
            &format!(
                "{} SELECT id FROM up WHERE {} ORDER BY depth LIMIT 1",
                up("true"),
                has_agent("up.id")
            ),
            params![id, agents.parameter()],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(here) = here {
        return Ok(Some(Receiver::Here(here)));
    }
    Ok(first_absent_above(db, id)?.map(Receiver::Elsewhere))
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
