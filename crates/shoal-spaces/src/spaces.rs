//! Spaces: workspaces and chats, which share one namespace of IDs and names,
//! so a request naming a space acts on whichever of them it names.
use anyhow::Result;
use rusqlite::OptionalExtension as _;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceName;

use crate::model::SpaceKind;

/// The kind of the space `id`, while it exists.
pub fn space_kind(db: &rusqlite::Connection, id: &SpaceId) -> Result<Option<SpaceKind>> {
    Ok(db
        .query_row("SELECT kind FROM spaces WHERE id=?1", [id], |row| {
            row.get(0)
        })
        .optional()?)
}

/// The name of the space `id`, while it exists.
pub fn space_name(db: &rusqlite::Connection, id: &SpaceId) -> Result<Option<SpaceName>> {
    Ok(db
        .query_row("SELECT name FROM spaces WHERE id=?1", [id], |row| {
            row.get(0)
        })
        .optional()?)
}

/// The IDs of every space, a workspace's or a chat's.
pub fn space_ids(db: &rusqlite::Connection) -> Result<std::collections::HashSet<SpaceId>> {
    Ok(db
        .prepare("SELECT id FROM spaces")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// The IDs of the workspaces of `repository`, or of every workspace.
pub fn workspace_ids(
    db: &rusqlite::Connection,
    repository: Option<&RepositoryId>,
) -> Result<Vec<SpaceId>> {
    Ok(db
        .prepare(
            "SELECT id FROM spaces WHERE kind='workspace' AND (?1 IS NULL OR repository_id=?1)",
        )?
        .query_map([repository], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

mod directories;
pub mod done;
pub mod environment;
pub mod export;
pub use export::{HELD_FOR_READER, MovePacking};
mod find;
mod hooks;
mod listing;
pub mod location;
pub use listing::worktrees_mut;
pub mod naming;
pub mod offers;
pub mod priority;
pub mod records;
pub mod removal_notices;
mod removing;
pub mod scopes;
pub use removing::{Removing, Reservation};
pub mod selection;
pub use selection::selected;
mod agent_sessions;
pub mod following;
pub mod sessions;
pub mod status;
