//! Places in terminal hosts: where Shoal runs work, recorded the same way for
//! every host so later work can find the place again, and whose a place is.
//! Generic code stores and passes the record; only the named host reads its
//! group, place and data.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use shoal_states::states;

states!(
    /// The terminal hosts Shoal runs work in, in the order they claim a
    /// terminal; a plain terminal is none, and every terminal falls back to
    /// it.
    HostName: Variants {
        Herdr => "herdr",
        Rex => "rex",
        Zmx => "zmx",
        Tmux => "tmux",
        Zellij => "zellij",
        Terminal => "terminal",
    }
);

impl std::str::FromStr for HostName {
    type Err = anyhow::Error;

    fn from_str(name: &str) -> anyhow::Result<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|host| host.as_str() == name)
            .ok_or_else(|| anyhow::anyhow!("unknown terminal host {name}"))
    }
}

/// A place in a terminal host, or the group a later place opens in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    pub host: HostName,
    /// The host's group of places, such as a Herdr workspace or a Rex
    /// session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// The place itself, such as a Herdr pane or a Rex block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place: Option<String>,
    /// What only the host reads, such as how to reach its server.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub data: BTreeMap<String, String>,
    /// Shoal created the place, so it may close it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub owned: bool,
    /// What tells the place apart from a later one under the same ID, for a
    /// host that reuses IDs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    /// The session of another host the work runs in, attached in this
    /// place, which other terminals attach to as well.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nested: Option<Box<Origin>>,
}

impl Origin {
    /// A place Shoal created in `host`, known by its ID alone.
    pub fn created(host: HostName, place: String) -> Self {
        Self {
            host,
            group: None,
            place: Some(place),
            data: BTreeMap::new(),
            owned: true,
            instance: None,
            nested: None,
        }
    }

    /// The place's ID; empty for a group alone.
    pub fn id(&self) -> &str {
        self.place.as_deref().unwrap_or_default()
    }

    /// Where the work in the place runs, which takes the keys Shoal types
    /// for it: the session nested in it, which takes them whether or not
    /// the place still shows it, else the place itself.
    pub fn into_typed(self) -> Origin {
        match self.nested {
            Some(nested) => *nested,
            None => self,
        }
    }

    /// The place itself and the session nested in it, which closing the
    /// place closes too.
    pub fn with_nested(&self) -> impl Iterator<Item = &Origin> {
        std::iter::once(self).chain(self.nested.as_deref())
    }
}

/// Whose a place is, as the daemon knows it when asked for a space.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaceOwnership {
    /// The space's: its record names the space, or its host labels it as
    /// the space's session, so the space's removal closes it.
    Space,
    /// Another space's agent took it over.
    Elsewhere,
    /// Neither recorded nor labelled, as a place opened before Shoal
    /// recorded its places.
    Unknown,
}

/// What happened to a recorded place a watcher asked the daemon to close.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaceClose {
    /// It was the workspace's, and is closed.
    Closed,
    /// Another workspace's agent or a chat took it over, so it stays open.
    OwnedElsewhere,
    /// It was the workspace's, and its host, whose places the daemon cannot
    /// close, closes it.
    HostCloses,
    /// The daemon keeps no record of it; its host closes it.
    NotRecorded,
}

#[cfg(test)]
mod tests;

/// A terminal host's session that `shoal attach` can reach, with the agent
/// running there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSession {
    pub name: String,
    /// The agent the daemon recorded running there, when one runs there.
    pub agent: Option<crate::agent::Agent>,
    /// Terminals attached to the session; a host that cannot count them
    /// gives one while it shows the session.
    pub clients: u32,
}

impl TerminalSession {
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
