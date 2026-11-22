//! Sharing a registered repository with the other machines of the hub.
use anyhow::Result;
use shoal_protocol::{hub::shared_repositories::Origin, model::Repository, repository::Selector};

use super::manager::Manager;

impl Manager {
    /// Share the repository `selector` names with the other machines of
    /// the hub, or stop sharing it, and have the hub hear of it, and of its
    /// local config, at once. One that comes to be shared takes the hub's
    /// copy of its local config.
    pub(super) async fn share_repository(
        &self,
        selector: impl Into<Selector>,
        shared: bool,
    ) -> Result<Repository> {
        let selector = selector.into();
        let before = self.repositories.repository(selector.clone()).await?;
        let repository = self.repositories.share(selector, shared).await?;
        if before.shared_url.is_none() {
            self.take_shared_local_config(&repository).await;
        }
        self.hub.shared_repositories_changed();
        self.hub.shared_local_configs_changed();
        Ok(repository)
    }

    /// Have `repository`, once shared, take the hub's copy of its local
    /// config.
    async fn take_shared_local_config(&self, repository: &Repository) {
        let Some(url) = &repository.shared_url else {
            return;
        };
        let origin = Origin::of(url);
        (self.hub)
            .shared_registration_joined(self, &repository.id, &origin)
            .await;
    }
}
