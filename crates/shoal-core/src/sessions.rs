//! Terminal sessions Shoal created, shared by the CLI and daemon. Places the
//! user opened stay theirs, even when Shoal runs an agent in them.
pub(crate) mod herdr;
mod locking;
pub(crate) mod rex;
pub(crate) mod tmux;
pub(crate) mod zellij;
pub(crate) mod zmx;

pub use crate::client::sessions::{HostName, Origin, PlaceClose, PlaceOwnership};

use crate::client::space_ids::SpaceId;
use anyhow::Result;
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path, sync::Arc};

/// A terminal session labelled with the workspace that owns it.
pub struct Session {
    pub name: String,
    pub workspace: SpaceId,
}

/// The ownership that must still match when a session is closed. Demo launches
/// have unique IDs even when their host reuses a session name in one workspace.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Owner {
    Workspace(SpaceId),
    Demo(String),
}

/// A host exposes its session operations; lifecycle policy belongs to the daemon.
/// Verified closes must exclude ownership replacement in the native namespace.
pub trait Host: Send + Sync {
    fn name(&self) -> HostName;
    /// Refuse when the program this host runs is missing.
    fn ensure_installed(&self) -> Result<()>;
    /// What names the server holding a place among this host's servers, as
    /// the place's `data` records it; empty for a host with one server.
    fn server(&self, _data: &BTreeMap<String, String>) -> String {
        String::new()
    }
    /// The sessions this host labels as Shoal's; a host without labels has
    /// only the places the daemon recorded.
    fn sessions<'a>(&'a self, state: &'a Path) -> BoxFuture<'a, Result<Vec<Session>>>;
    /// Close `place` while `owner` still owns it.
    fn close<'a>(
        &'a self,
        state: &'a Path,
        place: &'a Origin,
        owner: &'a Owner,
    ) -> BoxFuture<'a, Result<()>>;
}

/// Operations on the client type [`Origin`] that only the `shoal` command uses.
pub trait OriginExt {
    /// What names the server holding the place among its host's servers,
    /// as the host reads its data; empty for a host with one server. Places
    /// of different servers may share an ID.
    fn server(&self) -> String;
}

impl OriginExt for Origin {
    fn server(&self) -> String {
        host(self.host)
            .map(|host| host.server(&self.data))
            .unwrap_or_default()
    }
}

pub fn hosts() -> Vec<Arc<dyn Host>> {
    HostName::ALL.iter().copied().filter_map(host).collect()
}

/// Refuse when the program of host `name` is missing; the plain terminal
/// needs none.
pub fn ensure_installed(name: HostName) -> Result<()> {
    host(name).map_or(Ok(()), |host| host.ensure_installed())
}

/// The host named `name`, for its sessions and what it reads from its
/// places' records; none for the plain terminal, which keeps no sessions.
fn host(name: HostName) -> Option<Arc<dyn Host>> {
    match name {
        HostName::Herdr => Some(Arc::new(herdr::Cleanup)),
        HostName::Rex => Some(Arc::new(rex::Cleanup)),
        HostName::Zmx => Some(Arc::new(zmx::Zmx)),
        HostName::Tmux => Some(Arc::new(tmux::Cleanup)),
        HostName::Zellij => Some(Arc::new(zellij::Cleanup)),
        HostName::Terminal => None,
    }
}

#[cfg(test)]
mod tests;
