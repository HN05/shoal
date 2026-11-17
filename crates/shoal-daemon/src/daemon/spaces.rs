//! Spaces: workspaces and chats, which share one namespace of IDs and names,
//! so a request naming a space acts on whichever of them it names.
use anyhow::Result;
use shoal_ids::space_ids::SpaceId;

use crate::model::SpaceKind;

/// The kind of the space `id`, while it exists.
pub(crate) fn space_kind(db: &rusqlite::Connection, id: &SpaceId) -> Result<Option<SpaceKind>> {
    use rusqlite::OptionalExtension as _;
    Ok(db
        .query_row("SELECT kind FROM spaces WHERE id=?1", [id], |row| {
            row.get(0)
        })
        .optional()?)
}

pub(crate) mod done;
mod environment;
mod export;
mod find;
mod hooks;
mod naming;
pub(crate) mod priority;
pub(crate) mod records;
pub(crate) mod removal_notices;
mod removing;
mod scopes;
pub(crate) use removing::{Removing, Reservation};
mod selection;
pub(crate) use selection::selected;
pub(crate) mod sessions;
pub(crate) mod status;
