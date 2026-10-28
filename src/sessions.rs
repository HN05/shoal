//! Terminal session integrations shared by the CLI and daemon.
pub(crate) mod zmx;

use anyhow::Result;
use futures_util::future::BoxFuture;
use std::{path::Path, sync::Arc};

/// A terminal session labelled with the workspace that owns it.
pub(crate) struct Session {
    pub name: String,
    pub workspace: String,
}

/// A host exposes its session operations; lifecycle policy belongs to the daemon.
pub(crate) trait Host: Send + Sync {
    fn sessions<'a>(
        &'a self,
        state: &'a Path,
        include_legacy: bool,
    ) -> BoxFuture<'a, Result<Vec<Session>>>;
    fn close<'a>(&'a self, session: &'a str) -> BoxFuture<'a, Result<()>>;
}

pub(crate) fn hosts() -> Vec<Arc<dyn Host>> {
    // Unit tests inject hosts and never discover or close the user's sessions.
    if cfg!(test) {
        Vec::new()
    } else {
        vec![Arc::new(zmx::Zmx)]
    }
}
