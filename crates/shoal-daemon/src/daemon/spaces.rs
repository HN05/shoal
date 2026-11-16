//! Spaces: workspaces and chats, which share one namespace of IDs and names,
//! so a request naming a space acts on whichever of them it names.
use anyhow::Result;
use shoal_ids::space_ids::SpaceId;

use super::{workspace::Manager, workspace_files::Attacher};
use crate::{
    config::Effective,
    model::{Completion, Space, SpaceKind},
};
use shoal_protocol::{assignments::Done, space_names::SpaceSelector};

impl Manager {
    /// The settings of `space`: a workspace's own, or a chat's
    /// repository's.
    pub(crate) async fn present_space_settings(&self, space: &Space) -> Result<Effective> {
        match space.to_workspace() {
            Some(workspace) => self.workspace_settings(&workspace).await,
            None => self.space_settings_in(space.repository_id.as_ref()).await,
        }
    }

    /// Record the work of the workspace or running chat `selector` names as
    /// done, as [`Self::mark_done`] or [`Self::mark_chat_done`] does.
    pub async fn mark_space_done(
        &self,
        selector: &SpaceSelector,
        done: Done,
        attacher: &Attacher,
    ) -> Result<Completion> {
        let space = self.space(selector).await?;
        match space.kind() {
            SpaceKind::Workspace => {
                (self.mark_done(&SpaceSelector::id(&space.id), done, attacher)).await
            }
            SpaceKind::Chat => self.mark_chat_done(&space.id, done, attacher).await,
        }
    }

    /// Withdraw the completion the workspace or chat `selector`
    /// names recorded.
    pub async fn undo_space_done(&self, selector: &SpaceSelector) -> Result<Completion> {
        let space = self.space(selector).await?;
        match space.kind() {
            SpaceKind::Workspace => self.undo_done(&SpaceSelector::id(&space.id)).await,
            SpaceKind::Chat => self.undo_chat_done(&space.id).await,
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
