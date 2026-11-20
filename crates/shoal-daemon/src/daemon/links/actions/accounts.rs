//! The forge item actions go to, and the account a place changes items as.
use std::path::Path;

use anyhow::{Context, Result};

use crate::{
    daemon::{links::place::Place, manager::Manager},
    forge::ForgeRepo,
    model::{Repository, Workspace},
};
use shoal_forge::{RepoRequests, account::Account};

impl Manager {
    /// The forge of the `origin` of the checkout or worktree at `path`.
    pub(crate) async fn origin_forge(&self, path: &Path) -> Result<ForgeRepo> {
        let remote = crate::repository::remote_url_from_path(path)
            .await?
            .context("forge actions need an origin remote")?;
        self.forge(&remote).await
    }

    /// The forge of the workspace's `origin`.
    pub(in crate::daemon::links) async fn workspace_forge(
        &self,
        workspace: &Workspace,
    ) -> Result<ForgeRepo> {
        self.origin_forge(&workspace.path).await
    }

    /// The repository `named` names, as OWNER/NAME on the forge of `own` or
    /// as a repository URL on any forge, or `own` without one.
    pub(in crate::daemon::links) async fn named_forge(
        &self,
        own: ForgeRepo,
        named: Option<&str>,
    ) -> Result<ForgeRepo> {
        match named {
            Some(named) => own.resolve_named(named, &self.forge_hosts()).await,
            None => Ok(own),
        }
    }

    pub(crate) async fn workspace_forge_account(
        &self,
        workspace: &Workspace,
    ) -> Result<(ForgeRepo, Account)> {
        Ok((
            self.workspace_forge(workspace).await?,
            self.links.agent_account(),
        ))
    }

    /// The forge of the registered checkout's `origin`.
    pub(crate) async fn checkout_forge(&self, repository: &Repository) -> Result<ForgeRepo> {
        self.origin_forge(&repository.path).await
    }

    /// The forge of the registered checkout's `origin`, with the agent
    /// account.
    pub(in crate::daemon::links) async fn checkout_forge_account(
        &self,
        repository: &Repository,
    ) -> Result<(ForgeRepo, Account)> {
        Ok((
            self.checkout_forge(repository).await?,
            self.links.agent_account(),
        ))
    }

    /// The account that changes items from `place`: the agent account in a
    /// space, and the user's own logins in a registered checkout, where only
    /// the user acts.
    pub(in crate::daemon::links) fn place_account(&self, place: &Place) -> Account {
        match place {
            Place::Checkout(_) => self.links.user_account(),
            Place::Space(_) => self.links.agent_account(),
        }
    }
}
