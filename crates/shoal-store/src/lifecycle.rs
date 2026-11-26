//! Where a workspace's lifecycle stands. A lifecycle operation holds the
//! workspace in a transient state, which excludes every other operation.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::state::WorkspaceState;

/// How many times a workspace's lifecycle state has changed, which the
/// store counts on each change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Generation(i64);

pub struct Lifecycle {
    pub state: WorkspaceState,
    pub generation: Generation,
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
