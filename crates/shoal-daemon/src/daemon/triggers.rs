//! What `shoal-runner` needs of the rest of the daemon for its triggers and
//! its runner: the registered repositories and their config, the agents a
//! trigger's launch meets and starts, and the permission a scoped caller
//! acts with.
use std::collections::HashSet;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use futures_util::future::BoxFuture;
use shoal_ids::{queue_entry_ids::QueueEntryId, repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{
    config::permissions::PermissionKey,
    launches::{Launch, LaunchSource},
};
use shoal_runner::daemon::{Agents, Permissions, Repositories};

use super::{manager::Manager, permissions::Action, scope::Caller};
use crate::{
    config::{Config, Effective, repo::ConfigLayers},
    model::{QueueEntry, Repository},
    protocol::{ConfigTarget, ItemContext},
    state::QueueState,
};

impl Repositories for Manager {
    fn repositories(&self) -> BoxFuture<'_, Result<Vec<Repository>>> {
        Box::pin(Manager::repositories(self))
    }

    fn repository<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<Repository>> {
        Box::pin(Manager::repository(self, id))
    }

    fn repository_layers<'a>(
        &'a self,
        id: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<ConfigLayers>> {
        let target = ConfigTarget::Repository(id.clone().into());
        Box::pin(self.config_layers_for(target))
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
        Box::pin(Manager::repository_settings_from(self, config, id))
    }

    fn ensure_repository_available<'a>(
        &'a self,
        id: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(Manager::ensure_repository_available(self, id))
    }

    fn acts_on_global_config(&self) -> bool {
        Manager::acts_on_global_config(self)
    }
}

impl Agents for Manager {
    fn queue_entries(&self) -> BoxFuture<'_, Result<Vec<QueueEntry>>> {
        Box::pin(Manager::queue_entries(self))
    }

    fn live_queue_entries(&self) -> BoxFuture<'_, Result<HashSet<QueueEntryId>>> {
        Box::pin(async move {
            let entries = self.queue_entries().await?;
            Ok((entries.into_iter())
                .filter(|entry| entry.state != QueueState::Failed)
                .map(|entry| entry.id)
                .collect())
        })
    }

    fn started_launch<'a>(
        &'a self,
        entry: &'a QueueEntryId,
    ) -> BoxFuture<'a, Result<Option<SpaceId>>> {
        Box::pin(Manager::started_launch(self, entry))
    }

    fn job_could_start(&self) -> BoxFuture<'_, Result<bool>> {
        Box::pin(Manager::job_could_start(self))
    }

    fn agent_spaces(&self) -> BoxFuture<'_, Result<HashSet<SpaceId>>> {
        Box::pin(async move {
            let mut spaces = self.recorded_agent_spaces().await?;
            spaces.extend(self.queued_agent_workspaces().await?);
            Ok(spaces)
        })
    }

    fn launch(
        self: Arc<Self>,
        launch: Launch,
        source: LaunchSource,
    ) -> BoxFuture<'static, Result<SpaceId>> {
        Box::pin(async move {
            let launched = Manager::launch(&self, launch, source, None).await?;
            Ok(launched.space.id)
        })
    }
}

impl Permissions for Manager {
    fn authorize<'a>(
        &'a self,
        caller: Option<&'a Caller>,
        key: PermissionKey,
        action: String,
        repository: Option<RepositoryId>,
    ) -> BoxFuture<'a, Result<()>> {
        let mut authorized = Action::new(key, action);
        authorized.repository = repository;
        Box::pin(self.authorize_action(caller, authorized))
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
            ) => Some(Manager::repository(self, repository).await?.id),
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

#[cfg(test)]
mod tests;
