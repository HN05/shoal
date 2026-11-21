//! The space an attach returns to, as the daemon has it now, and the
//! sessions open where it recorded the space's agents.
use anyhow::Result;
use shoal_ids::space_ids::{SpaceId, SpaceNumber};
use shoal_protocol::{space_names::SpaceName, space_names::SpaceSelector, status::Inspection};

use crate::{
    client::connection::CommandPaths,
    model::{Chat, Protection},
    protocol::ConfigTarget,
    terminal::{self, Recorded, Session},
};

/// What `shoal attach` returns to.
pub enum Target {
    /// A workspace with the places its executions were recorded running in
    /// and those Shoal created for it.
    Workspace(Box<Inspection>),
    Chat(Box<Chat>),
}

impl Target {
    /// The workspace `workspace` selects.
    pub async fn inspected(paths: &CommandPaths, workspace: SpaceSelector) -> Result<Self> {
        let inspection = crate::client::workspaces::inspect(paths, workspace).await?;
        Ok(Self::Workspace(Box::new(inspection)))
    }

    /// The one of `chats` with the ID `id`, else the workspace with it.
    pub async fn among(paths: &CommandPaths, chats: Vec<Chat>, id: SpaceId) -> Result<Self> {
        if let Some(chat) = chats.into_iter().find(|chat| chat.id == id) {
            return Ok(Self::Chat(Box::new(chat)));
        }
        Self::inspected(paths, SpaceSelector::Id(id)).await
    }

    /// The chat or workspace with the ID `id`, as it is now.
    pub async fn by_id(paths: &CommandPaths, id: SpaceId) -> Result<Self> {
        let chats = crate::client::spaces::list(paths).await?.into_chats();
        Self::among(paths, chats, id).await
    }

    pub fn id(&self) -> &SpaceId {
        match self {
            Self::Workspace(inspection) => &inspection.workspace.id,
            Self::Chat(chat) => &chat.id,
        }
    }

    pub fn name(&self) -> &SpaceName {
        match self {
            Self::Workspace(inspection) => &inspection.workspace.name,
            Self::Chat(chat) => &chat.name,
        }
    }

    pub fn number(&self) -> SpaceNumber {
        match self {
            Self::Workspace(inspection) => inspection.workspace.number,
            Self::Chat(chat) => chat.number,
        }
    }

    /// The target's open sessions, each where an agent Shoal stopped waits
    /// marked with why.
    pub(super) async fn sessions(&self, paths: &CommandPaths) -> Result<Vec<Session>> {
        let settings = crate::settings::of(paths, self.config()).await?;
        let space = SpaceSelector::id(self.id());
        let places = crate::client::places::places(paths, space).await?;
        let recorded = Recorded::space(&places.executions, &places.places);
        terminal::sessions(&settings, &recorded).await
    }

    /// Whose settings choose the hosts to look in.
    fn config(&self) -> ConfigTarget {
        match self {
            Self::Workspace(inspection) => {
                ConfigTarget::Space(SpaceSelector::id(&inspection.workspace.id))
            }
            Self::Chat(chat) => ConfigTarget::chat(chat),
        }
    }

    /// Why there is nothing to attach to, and for a chat where it runs
    /// instead; for a user on another machine, that it runs on `machine`.
    pub(super) fn without_sessions(&self, machine: Option<&str>) -> String {
        let without = self.without_sessions_here();
        match machine {
            Some(machine) => format!(
                "{without}; it runs on machine {machine}: attach there, or move it to your machine with shoal move"
            ),
            None => without,
        }
    }

    fn without_sessions_here(&self) -> String {
        match self {
            Self::Workspace(inspection) => {
                format!("{} has no agent sessions", inspection.workspace.name)
            }
            Self::Chat(chat) => match chat.agent.as_ref().and_then(crate::agents::chat_place) {
                Some(place) => {
                    format!("{} runs in {place}; it has no session to attach", chat.name)
                }
                None => format!(
                    "{} has no session to attach; it runs in the terminal that started it",
                    chat.name
                ),
            },
        }
    }

    /// Why the target's agent, which `protection` stopped, cannot be
    /// attached to while no restore of it waits to start.
    pub(super) fn stopped_agent(&self, protection: &Protection) -> String {
        format!(
            "{}'s agent is stopped ({}) and resumes once {}",
            self.name(),
            protection.reason,
            protection.resumes_when,
        )
    }
}
