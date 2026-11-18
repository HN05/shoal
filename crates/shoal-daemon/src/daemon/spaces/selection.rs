//! Resolving the space a request names, by its number or ID, over
//! workspaces and chats alike, and refusing the other kind where a request
//! needs one.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension as _, params};
use shoal_ids::space_ids::SpaceId;

use crate::{
    daemon::{
        events::{self, EventCause, EventDetails},
        workspace::Manager,
    },
    model::{Chat, Space, SpaceKind, Workspace},
    refusal::{Reason, Refusal},
};
use shoal_protocol::space_names::SpaceSelector;

impl Manager {
    /// The workspace or chat, running or stopped, `selector` names.
    pub(crate) async fn space(&self, selector: &SpaceSelector) -> Result<Space> {
        let owned = selector.clone();
        let found = self.store.run(move |db| selected(db, &owned)).await?;
        let space = match found {
            // A removal may delete it once it is selected.
            Some((SpaceKind::Workspace, id)) => self.find_workspace(&id).await?.map(Space::from),
            Some((SpaceKind::Chat, id)) => self.chat_by_id(&id).await?.map(Space::from),
            None => None,
        };
        match space {
            Some(space) => Ok(space),
            None => Err(self.missing_space(selector).await),
        }
    }

    /// The refusal of a request for the space `selector` names, which does
    /// not exist here, as another joined machine's may: one selected by
    /// the ID of a removed space says it was removed, with its repository
    /// where that took it, while the journal keeps the removal.
    async fn missing_space(&self, selector: &SpaceSelector) -> anyhow::Error {
        let removal = match selector.given_id().cloned() {
            Some(id) => self.store.run(move |db| events::removal(db, &id)).await,
            None => Ok(None),
        };
        let message = match removal {
            Ok(Some(removal)) => removed(&removal),
            Ok(None) => format!("no space {selector}"),
            Err(error) => return error,
        };
        Refusal::absent(selector, message).into()
    }

    /// The workspace or chat, running or stopped, with the ID `id`, while
    /// it exists.
    pub(crate) async fn find_space(&self, id: &SpaceId) -> Result<Option<Space>> {
        let owned = id.to_owned();
        let kind = self
            .store
            .run(move |db| super::space_kind(db, &owned))
            .await?;
        Ok(match kind {
            Some(SpaceKind::Workspace) => self.find_workspace(id).await?.map(Space::from),
            Some(SpaceKind::Chat) => self.chat_by_id(id).await?.map(Space::from),
            None => None,
        })
    }

    /// The ID of the space `selector` names, when it names one.
    pub(crate) async fn space_id(
        &self,
        selector: Option<&SpaceSelector>,
    ) -> Result<Option<SpaceId>> {
        Ok(match selector {
            Some(selector) => Some(self.space(selector).await?.id.clone()),
            None => None,
        })
    }

    /// The workspace or chat, running or stopped, with the ID `id`.
    pub(crate) async fn space_by_id(&self, id: &SpaceId) -> Result<Space> {
        match self.find_space(id).await? {
            Some(space) => Ok(space),
            None => Err(self.missing_space(&SpaceSelector::id(id)).await),
        }
    }

    /// The workspace with the ID `id`, which is never read as a name,
    /// refusing a chat as [`Self::workspace`] does and a removed workspace
    /// as [`Self::space`] does.
    pub async fn workspace_by_id(&self, id: &SpaceId) -> Result<Workspace> {
        if let Some(workspace) = self.find_workspace(id).await? {
            return Ok(workspace);
        }
        (self.space_by_id(id).await?.into_workspace())
            .map_err(|chat| other_kind(&chat, SpaceKind::Workspace))
    }

    /// Whether a workspace or chat, running or stopped, has the ID `id`.
    pub(crate) async fn space_exists(&self, id: &SpaceId) -> Result<bool> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                crate::daemon::store::exists(db, "SELECT 1 FROM spaces WHERE id=?1", [id])
            })
            .await
    }

    /// The workspace `selector` names, refusing a chat.
    pub async fn workspace(&self, selector: &SpaceSelector) -> Result<Workspace> {
        (self.space(selector).await?.into_workspace())
            .map_err(|chat| other_kind(&chat, SpaceKind::Workspace))
    }

    /// The chat, running or stopped, `selector` names, refusing a
    /// workspace.
    pub(crate) async fn chat(&self, selector: &SpaceSelector) -> Result<Chat> {
        (self.space(selector).await?.into_chat())
            .map_err(|workspace| other_kind(&workspace, SpaceKind::Chat))
    }
}

/// The refusal of `space` to a request that needs a space of kind `needed`.
fn other_kind(space: &Space, needed: SpaceKind) -> anyhow::Error {
    let message = format!(
        "space {space} is a {}; this command needs a {}",
        space.kind(),
        needed.as_str()
    );
    Refusal::new(Reason::NotFound, message).into()
}

/// What `removal` of a space did, for a request that names it.
fn removed(removal: &EventDetails) -> String {
    let (id, name) = (&removal.space_id, &removal.name);
    match (removal.cause, &removal.repository_id) {
        (Some(EventCause::Repository), Some(repository)) => {
            format!("space {id} ({name}) was removed with its repository {repository}")
        }
        _ => format!("space {id} ({name}) was removed"),
    }
}

/// The kind and ID of the space `selector` names.
pub(crate) fn selected(
    db: &Connection,
    selector: &SpaceSelector,
) -> Result<Option<(SpaceKind, SpaceId)>> {
    let (id, number) = match selector {
        SpaceSelector::Number(number) => (None, Some(*number)),
        SpaceSelector::Id(id) => (Some(id), None),
        SpaceSelector::Previous => (None, None),
    };
    Ok(db
        .query_row(
            "SELECT kind, id FROM spaces WHERE id=?1 OR number=?2 LIMIT 1",
            params![id, number],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?)
}

#[cfg(test)]
mod tests;
