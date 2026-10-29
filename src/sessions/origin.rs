//! Where Shoal runs work in a terminal host, recorded the same way for every
//! host so later work can find the place again. Generic code stores and
//! passes the record; only the named host reads its group, place and data.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::state::states;

states!(
    /// The terminal hosts Shoal runs work in; a plain terminal is none.
    HostName: Variants {
        Terminal => "terminal",
        Herdr => "herdr",
        Rex => "rex",
        Zmx => "zmx",
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
        }
    }

    /// What names the server holding the place among its host's servers,
    /// as the host reads its data; empty for a host with one server. Places
    /// of different servers may share an ID.
    pub fn server(&self) -> String {
        match self.host {
            HostName::Herdr => super::herdr::server(&self.data),
            HostName::Rex => super::rex::server(&self.data),
            HostName::Terminal | HostName::Zmx => String::new(),
        }
    }

    /// The place's ID; empty for a group alone.
    pub fn id(&self) -> &str {
        self.place.as_deref().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests;
