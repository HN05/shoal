//! Sharing a registered repository with the other machines of the hub.
use anyhow::Result;
use shoal_protocol::{model::Repository, repository::Selector};

use super::manager::Manager;

impl Manager {
    /// Share the repository `selector` names with the other machines of
    /// the hub, or stop sharing it.
    pub(super) async fn share_repository(
        &self,
        selector: impl Into<Selector>,
        shared: bool,
    ) -> Result<Repository> {
        self.repositories.share(selector, shared).await
    }
}
