//! Terminal session integrations shared by the CLI and daemon.
//! The registry wires launch and cleanup capabilities together.
pub(crate) mod zmx;

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

/// A host exposes its session operations; lifecycle policy belongs to the daemon.
pub(crate) trait Host: Send + Sync {
    fn name(&self) -> &'static str;
    fn sessions<'a>(
        &'a self,
        state: &'a Path,
        include_legacy: bool,
    ) -> BoxFuture<'a, Result<Vec<Session>>>;
    fn close<'a>(
        &'a self,
        state: &'a Path,
        session: &'a str,
        owner: &'a Owner,
    ) -> BoxFuture<'a, Result<()>>;
}

// Each terminal host declares its cleanup adapter in this inventory. `None`
// is reserved for hosts without persistent places; policy never names a host.
macro_rules! registered_hosts {
    ($registry:ident) => {
        $registry! {
            (herdr, Herdr, None),
            (zmx, Zmx, Some(Arc::new(crate::sessions::zmx::Zmx) as Arc<dyn crate::sessions::Host>)),
        }
    };
}
pub(crate) use registered_hosts;

pub(crate) fn hosts() -> Vec<Arc<dyn Host>> {
    // Unit tests inject hosts and never discover or close the user's sessions.
    if cfg!(test) {
        Vec::new()
    } else {
        macro_rules! cleanup_hosts {
            ($(($module:ident, $host:ident, $cleanup:expr)),* $(,)?) => {
                [$($cleanup),*].into_iter().flatten().collect()
            };
        }
        registered_hosts!(cleanup_hosts)
    }
}
