//! The registered repositories and their config, and the repository a
//! request acts in.

use anyhow::{Context as _, Result};
use futures_util::future::BoxFuture;
use shoal_ids::repository_ids::RepositoryId;
use shoal_runner::daemon::Repositories;

use crate::{
    config::{Config, Effective, repo::ConfigLayers},
    daemon::manager::Manager,
    model::Repository,
    protocol::{ConfigTarget, ItemContext},
};

impl Repositories for Manager {
    fn repositories(&self) -> BoxFuture<'_, Result<Vec<Repository>>> {
        Box::pin(self.repositories.list())
    }

    fn repository<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<Repository>> {
        Box::pin(self.repositories.repository(id))
    }

    fn repository_layers<'a>(
        &'a self,
        id: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<ConfigLayers>> {
        let target = ConfigTarget::Repository(id.clone().into());
        Box::pin(self.repositories.config_layers_for(self, target))
    }

    fn context_repository(
        &self,
        context: Option<ItemContext>,
    ) -> BoxFuture<'_, Result<Option<RepositoryId>>> {
        Box::pin(self.context_repository(context))
    }

    fn repository_settings_from<'a>(
        &'a self,
        config: &'a Config,
        id: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(self.repositories.repository_settings_from(config, id))
    }

    fn ensure_repository_available<'a>(
        &'a self,
        id: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.repositories.ensure_available(id))
    }

    fn acts_on_global_config(&self) -> bool {
        Manager::acts_on_global_config(self)
    }
}

impl Manager {
    /// The registered repository `context` acts in: a workspace's or a
    /// chat's, or the checkout's; none without a context or for a chat
    /// without a repository.
    async fn context_repository(
        &self,
        context: Option<ItemContext>,
    ) -> Result<Option<RepositoryId>> {
        Ok(match context {
            None => None,
            Some(
                ItemContext::Checkout(repository)
                | ItemContext::Space {
                    repository: Some(repository),
                    ..
                },
            ) => Some(self.repositories.repository(repository).await?.id),
            Some(ItemContext::Space {
                space,
                repository: None,
            }) => {
                let space = self.space(&space).await;
                space.context("find the space to act in")?.repository_id
            }
        })
    }
}
