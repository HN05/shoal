//! Spaces: workspaces and chats, which share one namespace of IDs and names,
//! so a request naming a space acts on whichever of them it names.
use crate::client::space_ids::SpaceId;
use anyhow::Result;

use super::{workspace::Manager, workspace_files::Attacher};
use crate::{
    client::{assignments::Done, space_names::SpaceSelector},
    config::Effective,
    model::{Completion, PresentSpace, SpaceKind},
};

impl Manager {
    /// The settings of `space`: a workspace's own, or a chat's
    /// repository's.
    pub(crate) async fn present_space_settings(&self, space: &PresentSpace) -> Result<Effective> {
        match space {
            PresentSpace::Workspace(workspace) => self.workspace_settings(workspace).await,
            PresentSpace::Chat(chat) => self.chat_settings(chat).await,
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
        match self.space(selector).await? {
            PresentSpace::Workspace(workspace) => {
                (self.mark_done(&SpaceSelector::id(&workspace.id), done, attacher)).await
            }
            PresentSpace::Chat(chat) => self.mark_chat_done(&chat.id, done, attacher).await,
        }
    }

    /// Withdraw the completion the workspace or chat `selector`
    /// names recorded.
    pub async fn undo_space_done(&self, selector: &SpaceSelector) -> Result<Completion> {
        match self.space(selector).await? {
            PresentSpace::Workspace(workspace) => {
                self.undo_done(&SpaceSelector::id(&workspace.id)).await
            }
            PresentSpace::Chat(chat) => self.undo_chat_done(&chat.id).await,
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
pub(crate) mod priority;
pub(crate) mod removal_notices;
mod removing;
pub(crate) use removing::{Removing, Reservation};
mod selection;
pub(crate) use selection::selected;
mod sessions;
pub(crate) mod status;
