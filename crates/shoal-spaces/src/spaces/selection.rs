//! Resolving the space a request names, by its number or ID, over
//! workspaces and chats alike.
use crate::Spaces;
use crate::daemon::Daemon;
use crate::model::EventCause;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension as _, params};
use shoal_ids::space_ids::SpaceId;

use crate::{
    model::{Space, SpaceKind, Workspace},
    refusal::Refusal,
    {
        events::{self, EventDetails},
        target_checks::other_kind,
    },
};
use shoal_protocol::{model::Located, space_names::SpaceSelector};

impl Spaces {
    /// Running or stopped.
    pub async fn space(&self, selector: &SpaceSelector) -> Result<Space> {
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

    pub async fn holds_space(&self, selector: &SpaceSelector) -> Result<bool> {
        let owned = selector.clone();
        let found = self.store.read(move |db| selected(db, &owned)).await?;
        Ok(found.is_some())
    }

    /// Another joined machine may have it. A space selected by the ID of a
    /// removed space is said to be removed, with its repository where that
    /// took it, while the journal keeps the removal.
    pub async fn missing_space(&self, selector: &SpaceSelector) -> anyhow::Error {
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

    /// Running or stopped. None once it is gone.
    pub async fn find_space(&self, id: &SpaceId) -> Result<Option<Space>> {
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

    pub async fn space_id(&self, selector: Option<&SpaceSelector>) -> Result<Option<SpaceId>> {
        Ok(match selector {
            Some(selector) => Some(self.space(selector).await?.id.clone()),
            None => None,
        })
    }

    pub async fn space_id_anywhere(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
    ) -> Result<SpaceId> {
        Ok(self.space_anywhere(daemon, selector).await?.id().clone())
    }

    /// One here, else one another joined machine runs, as the hub lists it.
    /// One neither has is refused as absent. The daemon's own operations
    /// reach a space through this once the caller's reach was checked. A
    /// request's own target is found with [`Self::locate_space`], among the
    /// spaces its caller sees.
    pub async fn space_anywhere(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
    ) -> Result<Located> {
        let missing = match self.space(selector).await {
            Ok(space) => return Ok(Located::Here(Box::new(space))),
            Err(missing) => missing,
        };
        match daemon.machine_of(selector).await {
            Ok(Some((machine, listed))) => Ok(Located::Elsewhere {
                space: Box::new(listed),
                machine: Box::new(machine),
            }),
            _ => Err(missing),
        }
    }

    pub async fn space_by_id(&self, id: &SpaceId) -> Result<Space> {
        match self.find_space(id).await? {
            Some(space) => Ok(space),
            None => Err(self.missing_space(&SpaceSelector::id(id)).await),
        }
    }

    /// Never reads `id` as a name. Refuses a chat as [`Self::workspace`]
    /// does, and a removed workspace as [`Self::space`] does.
    pub async fn workspace_by_id(&self, id: &SpaceId) -> Result<Workspace> {
        if let Some(workspace) = self.find_workspace(id).await? {
            return Ok(workspace);
        }
        (self.space_by_id(id).await?.into_workspace())
            .map_err(|chat| other_kind(&chat, SpaceKind::Workspace))
    }

    pub async fn space_exists(&self, id: &SpaceId) -> Result<bool> {
        let id = id.to_owned();
        self.store
            .run(move |db| crate::store::exists(db, "SELECT 1 FROM spaces WHERE id=?1", [id]))
            .await
    }

    /// Refuses a chat.
    pub async fn workspace(&self, selector: &SpaceSelector) -> Result<Workspace> {
        (self.space(selector).await?.into_workspace())
            .map_err(|chat| other_kind(&chat, SpaceKind::Workspace))
    }

    /// Refuses a workspace.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn chat(&self, selector: &SpaceSelector) -> Result<crate::model::Chat> {
        (self.space(selector).await?.into_chat())
            .map_err(|workspace| other_kind(&workspace, SpaceKind::Chat))
    }
}

fn removed(removal: &EventDetails) -> String {
    let (id, name) = (&removal.space_id, &removal.name);
    match (removal.cause, &removal.repository_id) {
        (Some(EventCause::Repository), Some(repository)) => {
            format!("space {id} ({name}) was removed with its repository {repository}")
        }
        _ => format!("space {id} ({name}) was removed"),
    }
}

pub fn selected(db: &Connection, selector: &SpaceSelector) -> Result<Option<(SpaceKind, SpaceId)>> {
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
