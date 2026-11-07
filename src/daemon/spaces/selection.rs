//! Resolving the space a request names, by its number, ID or name, over
//! workspaces and chats alike, and refusing the other kind where a request
//! needs one.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension as _, params};

use crate::{
    client::space_names::SpaceSelector,
    daemon::workspace::Manager,
    model::{Chat, PresentSpace, SpaceKind, Workspace},
    refusal::{Reason, Refusal},
};

impl Manager {
    /// The workspace or chat, running or stopped, `selector` names.
    pub(crate) async fn space(&self, selector: &SpaceSelector) -> Result<PresentSpace> {
        let owned = selector.clone();
        let found = self.store.run(move |db| selected(db, &owned)).await?;
        let space = match found {
            Some((SpaceKind::Workspace, id)) => {
                Some(PresentSpace::Workspace(self.workspace_by_id(&id).await?))
            }
            Some((SpaceKind::Chat, id)) => self.chat_by_id(&id).await?.map(PresentSpace::Chat),
            None => None,
        };
        space.ok_or_else(|| Refusal::new(Reason::NotFound, format!("no space {selector}")).into())
    }

    /// The ID of the space `selector` names, when it names one.
    pub(crate) async fn space_id(
        &self,
        selector: Option<&SpaceSelector>,
    ) -> Result<Option<String>> {
        Ok(match selector {
            Some(selector) => Some(self.space(selector).await?.id().to_owned()),
            None => None,
        })
    }

    /// The workspace or chat, running or stopped, with the ID `id`.
    pub(crate) async fn space_by_id(&self, id: &str) -> Result<PresentSpace> {
        if let Some(workspace) = self.find_workspace(id).await? {
            return Ok(PresentSpace::Workspace(workspace));
        }
        let chat = self.chat_by_id(id).await?;
        chat.map(PresentSpace::Chat)
            .ok_or_else(|| Refusal::new(Reason::NotFound, format!("no space {id}")).into())
    }

    /// The workspace `selector` names, refusing a chat.
    pub async fn workspace(&self, selector: &SpaceSelector) -> Result<Workspace> {
        match self.space(selector).await? {
            PresentSpace::Workspace(workspace) => Ok(workspace),
            chat => Err(other_kind(&chat, SpaceKind::Workspace)),
        }
    }

    /// The running chat `selector` names.
    pub(crate) async fn running_chat(&self, selector: &SpaceSelector) -> Result<Chat> {
        let chat = self.chat(selector).await?;
        anyhow::ensure!(chat.restore.is_none(), "no running chat {selector}");
        Ok(chat)
    }

    /// The chat, running or stopped, `selector` names, refusing a
    /// workspace.
    pub(crate) async fn chat(&self, selector: &SpaceSelector) -> Result<Chat> {
        match self.space(selector).await? {
            PresentSpace::Chat(chat) => Ok(chat),
            workspace => Err(other_kind(&workspace, SpaceKind::Chat)),
        }
    }
}

/// The refusal of `space` to a request that needs a space of kind `needed`.
fn other_kind(space: &PresentSpace, needed: SpaceKind) -> anyhow::Error {
    let message = format!(
        "space {space} is a {}; this command needs a {}",
        space.kind(),
        needed.as_str()
    );
    Refusal::new(Reason::NotFound, message).into()
}

/// The kind and ID of the space `selector` names.
pub(crate) fn selected(
    db: &Connection,
    selector: &SpaceSelector,
) -> Result<Option<(SpaceKind, String)>> {
    let (id, name, number) = match selector {
        SpaceSelector::Number(number) => (None, None, Some(*number)),
        SpaceSelector::IdOrName(text) => (Some(text.as_str()), Some(text.as_str()), None),
        SpaceSelector::Id(id) => (Some(id.as_str()), None, None),
    };
    Ok(db
        .query_row(
            "SELECT 'workspace', id FROM workspaces WHERE id=?1 OR name=?2 OR number=?3
                UNION ALL SELECT 'chat', id FROM chats WHERE id=?1 OR name=?2 OR number=?3
                LIMIT 1",
            params![id, name, number],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?)
}

#[cfg(test)]
mod tests;
