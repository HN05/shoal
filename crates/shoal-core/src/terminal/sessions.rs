//! The agent sessions hosts keep, whom each runs for, and whether a
//! terminal shows one.
use serde::Serialize;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;

use crate::{
    model::Protection,
    sessions::{HostName, Origin},
};

/// What an agent session runs for, by ID; a host that keeps ownership in its
/// sessions labels them with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner<'a> {
    Workspace(&'a SpaceId),
    Chat(&'a SpaceId),
}

/// What kind of [`Owner`] a session has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OwnerKind {
    Workspace,
    Chat,
}

impl<'a> Owner<'a> {
    pub(crate) fn kind(self) -> OwnerKind {
        match self {
            Self::Workspace(_) => OwnerKind::Workspace,
            Self::Chat(_) => OwnerKind::Chat,
        }
    }

    pub fn id(self) -> &'a SpaceId {
        match self {
            Self::Workspace(id) | Self::Chat(id) => id,
        }
    }
}

/// The name of a session the chat `chat` runs in. A chat's name is a label
/// nothing is built from, so the session takes an opaque one, and
/// `shoal attach` finds the chat by the session's label.
pub fn chat_session_name(chat: &SpaceId) -> String {
    let id: String = chat.as_str().chars().take(8).collect();
    format!("chat-{id}")
}

/// A terminal host's session that `shoal attach` can reach, with the agent
/// running there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Session {
    pub name: String,
    /// The agent the daemon recorded running there, when one runs there.
    pub agent: Option<crate::agent::Agent>,
    /// Terminals attached to the session; a host that cannot count them
    /// gives one while it shows the session.
    pub clients: u32,
    /// The execution the daemon recorded there.
    #[serde(skip)]
    pub execution: Option<ExecutionId>,
    /// Why Shoal stopped the agent there, while it waits to be restored.
    #[serde(skip)]
    pub stopped: Option<Protection>,
    /// Where it was recorded, which names the host keeping it.
    #[serde(skip)]
    pub place: Origin,
}

impl Session {
    /// The session `name` a host found open at `place`, with `clients`
    /// attached; the daemon's records supply its agent.
    pub fn open(name: String, clients: u32, place: &Origin) -> Self {
        Self {
            name,
            agent: None,
            clients,
            execution: None,
            stopped: None,
            place: place.clone(),
        }
    }

    /// The host and the name it knows the session by, the same for every
    /// place it found open in the session.
    pub fn key(&self) -> (HostName, &str) {
        (self.place.host, &self.name)
    }

    /// The session as commands print it.
    pub fn output(self) -> shoal_protocol::sessions::TerminalSession {
        shoal_protocol::sessions::TerminalSession {
            name: self.name,
            agent: self.agent,
            clients: self.clients,
        }
    }

    /// The name, agent, whether Shoal stopped the agent, and whether a
    /// terminal is attached.
    pub fn label(&self) -> String {
        let mut label = self.name.clone();
        if let Some(agent) = &self.agent {
            label.push_str(&format!("  {agent}"));
        }
        if self.stopped.is_some() {
            label.push_str("  stopped");
        }
        if self.clients > 0 {
            label.push_str("  attached");
        }
        label
    }
}

#[cfg(test)]
mod tests;
