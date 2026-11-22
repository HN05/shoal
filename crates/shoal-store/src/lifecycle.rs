//! Where a workspace's lifecycle stands: the transient state a lifecycle
//! operation holds it in, which excludes every other, and how many times
//! that state has changed.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use shoal_ids::space_ids::SpaceId;

use crate::state::WorkspaceState;

/// The refusal of a workspace held in a lifecycle state that passes, which
/// a caller may wait out.
#[derive(Debug)]
pub struct Reserved(pub WorkspaceState);

impl std::fmt::Display for Reserved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "workspace is {}", self.0)
    }
}

impl std::error::Error for Reserved {}

/// How many times a workspace's lifecycle state has changed, which the
/// store counts on each change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Generation(i64);

/// Where a workspace's lifecycle stands.
pub struct Lifecycle {
    pub state: WorkspaceState,
    pub generation: Generation,
}

/// Refuse the workspace with [`Reserved`] while a lifecycle operation that
/// passes holds it.
pub fn ensure_unreserved(db: &Connection, id: &SpaceId) -> Result<()> {
    match lifecycle(db, id)? {
        Some(Lifecycle { state, .. }) if state.passes() => Err(Reserved(state).into()),
        _ => Ok(()),
    }
}

/// Where the lifecycle of the workspace `id` stands, while it exists.
pub fn lifecycle(db: &Connection, id: &SpaceId) -> Result<Option<Lifecycle>> {
    Ok(db
        .query_row(
            "SELECT state, lifecycle_generation FROM workspaces WHERE id=?1",
            [id],
            |row| {
                Ok(Lifecycle {
                    state: row.get(0)?,
                    generation: Generation(row.get(1)?),
                })
            },
        )
        .optional()?)
}
