//! Terminal sessions Shoal created, shared by the CLI and daemon. Places the
//! user opened stay theirs, even when Shoal runs an agent in them.
mod locking;
mod origin;
pub(crate) mod rex;
pub(crate) mod zmx;

pub(crate) use origin::{HostName, Origin};

use anyhow::Result;
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::Arc};

/// A terminal session labelled with the workspace that owns it.
pub(crate) struct Session {
    pub name: String,
    pub workspace: String,
}

/// The ownership that must still match when a session is closed. Demo launches
/// have unique IDs even when their host reuses a session name in one workspace.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Owner {
    Workspace(String),
    Demo(String),
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

/// A host exposes its session operations; lifecycle policy belongs to the daemon.
/// Verified closes must exclude ownership replacement in the native namespace.
pub(crate) trait Host: Send + Sync {
    fn name(&self) -> HostName;
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

pub(crate) fn hosts() -> Vec<Arc<dyn Host>> {
    // Unit tests inject hosts and never discover or close the user's sessions.
    if cfg!(test) {
        Vec::new()
    } else {
        vec![Arc::new(zmx::Zmx), Arc::new(rex::Cleanup)]
    }
}
