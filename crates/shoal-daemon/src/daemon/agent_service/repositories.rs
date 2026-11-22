//! The registered repositories, whose paths agents trust.
use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_agents::daemon::Repositories;
use shoal_protocol::protocol::ConfigTarget;

use crate::config::Effective;
use crate::config::repo::ConfigLayers;
use crate::daemon::manager::Manager;
use crate::model::Repository;
use crate::repository::Selector;
use shoal_ids::repository_ids::RepositoryId;

impl Repositories for Manager {
    fn repository<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<Repository>> {
        Box::pin(self.repositories.repository(id))
    }

    fn selected_repository<'a>(
        &'a self,
        selector: &'a Selector,
    ) -> BoxFuture<'a, Result<Repository>> {
        Box::pin(self.repositories.repository(selector.clone()))
    }

    fn repository_settings<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(self.repositories.repository_settings(id))
    }

    fn repository_layers<'a>(
        &'a self,
        id: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<ConfigLayers>> {
        Box::pin(
            self.repositories
                .config_layers_for(self, ConfigTarget::Repository(id.clone().into())),
        )
    }
}
