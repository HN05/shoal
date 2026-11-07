//! Spaces: workspaces and chats, which share one namespace of IDs and names,
//! so a request naming a space acts on whichever of them it names.
use anyhow::{Context as _, Result};

use super::{workspace::Manager, workspace_files::Attacher};
use crate::{
    client::{assignments::Done, space_names::SpaceSelector},
    config::Effective,
    daemon::notifications::NotificationKind,
    model::{Completion, PresentSpace, SpaceKind},
};

/// The space a message for another space's agent is about.
#[derive(Debug, Clone, Copy)]
pub(crate) struct About<'a> {
    pub name: &'a str,
    pub id: Option<&'a str>,
    /// The notification the event would be for the user.
    pub kind: Option<NotificationKind>,
}

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

    /// Queue `message` for the agent of the space `id`, with Shoal's copies
    /// `attachments` of files another space's files already keep, `about`
    /// another space when it concerns one. A space whose agent is not
    /// running receives it once its agent starts. It reads the space as
    /// recorded, stopping no chat, so a caller holding the approval
    /// announcement gate may queue one.
    pub(crate) async fn queue_space_message(
        &self,
        id: &str,
        about: Option<About<'_>>,
        message: String,
        attachments: Vec<std::path::PathBuf>,
    ) -> Result<()> {
        let space = id.to_owned();
        let kind = self.store.run(move |db| space_kind(db, &space)).await?;
        match kind.with_context(|| format!("no space {id}"))? {
            SpaceKind::Workspace => {
                let message = match about {
                    Some(about) => format!("{}: {message}", about.name),
                    None => message,
                };
                (self.queue_agent_message_attached(id, message, attachments)).await
            }
            SpaceKind::Chat => {
                let (name, about_id, kind) = match about {
                    Some(about) => (Some(about.name), about.id, about.kind),
                    None => (None, None, None),
                };
                (self.queue_chat_message(id, name, about_id, kind, message, attachments)).await
            }
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
pub(crate) fn space_kind(db: &rusqlite::Connection, id: &str) -> Result<Option<SpaceKind>> {
    use rusqlite::OptionalExtension as _;
    Ok(db
        .query_row(
            "SELECT 'workspace' FROM workspaces WHERE id=?1
                UNION ALL SELECT 'chat' FROM chats WHERE id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()?)
}

mod export;
mod find;
mod selection;
pub(crate) use selection::selected;
mod sessions;
pub(crate) mod status;
