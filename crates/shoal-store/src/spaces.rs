//! The reads of a space's own row, its workspace's and its completion that
//! the subsystems share.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Row};
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{
    model::{SpaceKind, Workspace},
    state::WorkspaceState,
};

use crate::{WORKSPACE_COLUMNS, exists};

/// What a space records of its kind, workspace lifecycle and completion.
#[derive(Clone, Debug)]
pub struct SpaceFacts {
    pub id: SpaceId,
    pub kind: SpaceKind,
    pub repository: Option<RepositoryId>,
    pub name: String,
    /// A workspace's lifecycle state; none for a chat.
    pub state: Option<WorkspaceState>,
    pub setup_finished: bool,
    pub done: bool,
}

impl SpaceFacts {
    /// Whether agents work there now: a chat, or a ready workspace.
    pub fn open(&self) -> bool {
        self.state
            .is_none_or(|state| state == WorkspaceState::Ready)
    }
}

/// What [`space_facts`] reads of a space, before its condition.
const SPACE_FACTS: &str = "SELECT s.id,s.kind,s.repository_id,s.name,w.state,
    COALESCE(w.setup_finished,0),
    EXISTS (SELECT 1 FROM space_completion c WHERE c.space_id=s.id)
    FROM spaces s LEFT JOIN workspaces w USING(id)";

fn space_facts(row: &Row<'_>) -> rusqlite::Result<SpaceFacts> {
    Ok(SpaceFacts {
        id: row.get(0)?,
        kind: row.get(1)?,
        repository: row.get(2)?,
        name: row.get(3)?,
        state: row.get(4)?,
        setup_finished: row.get(5)?,
        done: row.get(6)?,
    })
}

/// The facts of every space, in the order the spaces were recorded.
pub fn all_facts(db: &Connection) -> Result<Vec<SpaceFacts>> {
    Ok(db
        .prepare(&format!("{SPACE_FACTS} ORDER BY s.rowid"))?
        .query_map([], space_facts)?
        .collect::<rusqlite::Result<_>>()?)
}

/// The facts of the space `id`, while it exists.
pub fn facts(db: &Connection, id: &SpaceId) -> Result<Option<SpaceFacts>> {
    Ok(db
        .query_row(&format!("{SPACE_FACTS} WHERE s.id=?1"), [id], space_facts)
        .optional()?)
}

/// The workspace `id`, while it exists.
pub fn find_workspace(db: &Connection, id: &SpaceId) -> Result<Option<Workspace>> {
    Ok(db
        .query_row(
            &format!(
                "SELECT {WORKSPACE_COLUMNS} FROM spaces JOIN workspaces USING(id) WHERE id=?1"
            ),
            [id],
            crate::workspace,
        )
        .optional()?)
}

pub fn is_workspace(db: &Connection, id: &SpaceId) -> Result<bool> {
    exists(
        db,
        "SELECT 1 FROM spaces WHERE id=?1 AND kind='workspace'",
        [id],
    )
}

pub fn is_chat(db: &Connection, id: &SpaceId) -> Result<bool> {
    exists(db, "SELECT 1 FROM spaces WHERE id=?1 AND kind='chat'", [id])
}

/// Whether `shoal done` recorded the space `id`'s work as finished.
pub fn done(db: &Connection, id: &SpaceId) -> Result<bool> {
    exists(db, "SELECT 1 FROM space_completion WHERE space_id=?1", [id])
}
