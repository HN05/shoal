//! The repositories the spaces' workspaces belong to: their checkouts,
//! settings, seeds and syncs.
use std::path::{Path, PathBuf};

use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::cleanup::Emptying;
use shoal_protocol::model::{CheckoutSync, Repository, Space, SyncedRepository, Workspace};
use shoal_protocol::protocol::ConfigTarget;
use tokio::sync::OwnedMutexGuard;

use crate::config::repo::ConfigLayers;
use crate::config::{Config, Effective};
use crate::daemon::manager::Manager;
use crate::repository::Selector;

impl shoal_spaces::daemon::Repositories for Manager {
    fn apply_seed<'a>(
        &'a self,
        repo: &'a Repository,
        workspace: &'a Workspace,
        settings: &'a Effective,
    ) -> BoxFuture<'a, ()> {
        Box::pin(
            self.repositories
                .apply_seed(self, repo, workspace, settings),
        )
    }

    fn checkout_sync(&self, repository_id: &RepositoryId) -> Option<CheckoutSync> {
        self.repositories.checkout_sync(repository_id)
    }

    fn destroy_seed_outputs<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, ()> {
        Box::pin(self.repositories.destroy_seed_outputs(self, workspace))
    }

    fn ensure_available<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.repositories.ensure_available(id))
    }

    fn fetch_default_branch<'a>(
        &'a self,
        repository_id: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.repositories.fetch_default_branch(self, repository_id))
    }

    fn default_branch_fresh<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, bool> {
        Box::pin(self.repositories.default_branch_fresh(self, id))
    }

    fn empty_caches<'a>(
        &'a self,
        repo: &'a Repository,
        dry_run: bool,
    ) -> BoxFuture<'a, Result<Option<Emptying>>> {
        Box::pin(self.repositories.empty_caches(repo, dry_run))
    }

    fn forget_rejected_config(&self, id: &SpaceId) {
        self.repositories.forget_rejected_config(id)
    }

    fn lock_repository(
        &self,
        selector: Selector,
    ) -> BoxFuture<'_, Result<(Repository, OwnedMutexGuard<()>)>> {
        Box::pin(self.repositories.lock(selector))
    }

    fn verified_seed_worktree<'a>(
        &'a self,
        repo: &'a Repository,
        root: &'a Path,
    ) -> BoxFuture<'a, Option<PathBuf>> {
        Box::pin(async move {
            let seed = shoal_repositories::seed::Layout::new(root, &repo.id);
            let verified = shoal_repositories::seed::is_seed_worktree(repo, &seed).await;
            verified.unwrap_or(false).then(|| seed.worktree())
        })
    }

    fn prepare_chat_root(&self) -> BoxFuture<'_, Result<PathBuf>> {
        Box::pin(self.repositories.prepare_chat_root())
    }

    fn register_repository(
        &self,
        source: Selector,
        name: Option<String>,
        clone_path: Option<PathBuf>,
    ) -> BoxFuture<'_, Result<Repository>> {
        Box::pin(self.repositories.register(source, name, clone_path))
    }

    fn repository_settings<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(self.repositories.repository_settings(id))
    }

    fn repository_settings_with_templates<'a>(
        &'a self,
        id: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(self.repositories.repository_settings_with_templates(id))
    }

    fn space_settings<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(self.repositories.space_settings(self, space))
    }

    fn space_settings_with_templates<'a>(
        &'a self,
        space: &'a Space,
        repository: Option<&'a RepositoryId>,
    ) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(
            self.repositories
                .space_settings_with_templates(self, space, repository),
        )
    }

    fn sync_repository<'a>(
        &'a self,
        selector: &'a Selector,
    ) -> BoxFuture<'a, Result<SyncedRepository>> {
        Box::pin(self.repositories.sync_repository(self, selector))
    }

    fn target_settings_with_templates(
        &self,
        target: ConfigTarget,
    ) -> BoxFuture<'_, Result<Effective>> {
        Box::pin(
            self.repositories
                .target_settings_with_templates(self, target),
        )
    }

    fn tracked_config<'a>(&'a self, repository: &'a Repository) -> BoxFuture<'a, Result<String>> {
        Box::pin(self.repositories.tracked_config(repository))
    }

    fn tracked_settings(&self, tracked: &str) -> Result<Effective> {
        self.repositories.tracked_settings(tracked)
    }

    fn workspace_layers<'a>(
        &'a self,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<ConfigLayers>> {
        Box::pin(self.repositories.workspace_layers(self, workspace))
    }

    fn workspace_settings<'a>(
        &'a self,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(self.repositories.workspace_settings(self, workspace))
    }

    fn workspace_settings_from<'a>(
        &'a self,
        config: &'a Config,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(
            self.repositories
                .workspace_settings_from(self, config, workspace),
        )
    }

    fn workspace_settings_with_templates<'a>(
        &'a self,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(
            self.repositories
                .workspace_settings_with_templates(self, workspace),
        )
    }
}
