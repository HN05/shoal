//! The agent sessions hosts keep, and whom each runs for.
use serde::Serialize;

use crate::{
    protocol::Protection,
    sessions::{HostName, Origin},
};

/// What an agent session runs for, by ID; a host that keeps ownership in its
/// sessions labels them with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Owner<'a> {
    Workspace(&'a str),
    Chat(&'a str),
}

/// What kind of [`Owner`] a session has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OwnerKind {
    Workspace,
    Chat,
}

impl<'a> Owner<'a> {
    pub fn kind(self) -> OwnerKind {
        match self {
            Self::Workspace(_) => OwnerKind::Workspace,
            Self::Chat(_) => OwnerKind::Chat,
        }
    }

    pub fn id(self) -> &'a str {
        match self {
            Self::Workspace(id) | Self::Chat(id) => id,
        }
    }
}

/// A running agent session that `shoal attach` can reach.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Session {
    pub name: String,
    /// The agent the daemon recorded running there, when one runs there.
    pub agent: Option<String>,
    /// Terminals attached to the session; a host that cannot count them
    /// gives one while it shows the session.
    pub clients: u32,
    /// Why Shoal stopped the agent there, while it waits to be restored.
    #[serde(skip)]
    pub stopped: Option<Protection>,
    /// Where it was recorded, which names the host keeping it.
    #[serde(skip)]
    pub(crate) place: Origin,
}

impl Session {
    /// The session `name` a host found open at `place`, with `clients`
    /// attached; the daemon's records supply its agent.
    pub(crate) fn open(name: String, clients: u32, place: &Origin) -> Self {
        Self {
            name,
            agent: None,
            clients,
            stopped: None,
            place: place.clone(),
        }
    }

    /// The host and the name it knows the session by, the same for every
    /// place it found open in the session.
    pub(crate) fn key(&self) -> (HostName, &str) {
        (self.place.host, &self.name)
    }

    /// The session as commands print it.
    pub fn output(self) -> crate::client::output::status::AgentSession {
        crate::client::output::status::AgentSession {
            name: self.name,
            agent: self.agent,
            clients: self.clients,
        }
    }

    /// The name, agent, and whether a terminal is attached.
    pub fn label(&self) -> String {
        let mut label = self.name.clone();
        if let Some(agent) = &self.agent {
            label.push_str(&format!("  {agent}"));
        }
        if self.clients > 0 {
            label.push_str("  attached");
        }
        label
    }
}
