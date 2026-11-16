//! Spaces: workspaces and chats, which share one namespace of IDs and names,
//! so a request naming a space acts on whichever of them it names.
use anyhow::Result;
use shoal_ids::space_ids::SpaceId;

use super::workspace::Manager;
use crate::{
    config::Effective,
    model::{Space, SpaceKind},
};

impl Manager {
    /// The settings of `space`: a workspace's own, or a chat's
    /// repository's.
    pub(crate) async fn present_space_settings(&self, space: &Space) -> Result<Effective> {
        match space.to_workspace() {
            Some(workspace) => self.workspace_settings(&workspace).await,
            None => self.space_settings_in(space.repository_id.as_ref()).await,
        }
    }
}

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
mod export;
mod find;
mod naming;
pub(crate) mod priority;
pub(crate) mod records;
pub(crate) mod removal_notices;
mod removing;
pub(crate) use removing::{Removing, Reservation};
mod selection;
pub(crate) use selection::selected;
pub(crate) mod sessions;
pub(crate) mod status;
