//! The forge and the accounts item actions go out with: the user's own
//! logins for reads and the user's decisions, the review account for a
//! scoped caller's verdicts, the agent account for the rest.
use std::path::Path;

use anyhow::{Context, Result};

use crate::{
    config::{
        Config,
        forges::{Forges, Whose},
    },
    daemon::{links::place::Place, manager::Manager},
    forge::ForgeRepo,
    model::{Repository, Workspace},
};
use shoal_forge::{RepoRequests, account::Account};

/// What names the account of the user's own tokens, `forges` sets, in
/// forge memories.
fn user_identity(forges: &Forges) -> String {
    format!("user {}", forges.identity(Whose::User))
}

impl Manager {
    /// The user's own logins, which reads and the user's decisions use.
    pub(crate) fn user_account(&self) -> Account {
        let forges = &self.config().machine.forges;
        Account::user(forges.logins(Whose::User))
            .remembering(self.links.forge_memory(&user_identity(forges)))
            .naming_missing(Whose::User.missing())
    }

    /// The agent account of the current configuration.
    pub(crate) fn agent_account(&self) -> Account {
        self.agent_account_from(&self.config())
    }

    /// The agent account of `config`: the hosts' agent tokens, and the
    /// user's own logins for hosts without one.
    pub(in crate::daemon::links) fn agent_account_from(&self, config: &Config) -> Account {
        let forges = &config.machine.forges;
        // An agent with the user's logins shares what the user's requests taught.
        let tokens = forges.identity(Whose::Agent);
        let user = user_identity(forges);
        let identity = if tokens.is_empty() {
            user
        } else {
            format!("agent {tokens} {user}")
        };
        Account::agent(forges.logins(Whose::Agent), forges.logins(Whose::User))
            .remembering(self.links.forge_memory(&identity))
            .naming_missing(Whose::Agent.missing())
    }

    /// The account scoped callers approve and request changes on PRs as:
    /// the hosts' review tokens, with no other login to fall back on.
    pub(in crate::daemon::links) fn review_account(&self) -> Account {
        let forges = &self.config().machine.forges;
        let identity = format!("review {}", forges.identity(Whose::Review));
        Account::user(forges.logins(Whose::Review))
            .remembering(self.links.forge_memory(&identity))
            .naming_missing(Whose::Review.missing())
    }

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
        Ok((self.workspace_forge(workspace).await?, self.agent_account()))
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
        Ok((self.checkout_forge(repository).await?, self.agent_account()))
    }

    /// The account that changes items from `place`: the agent account in a
    /// space, and the user's own logins in a registered checkout, where only
    /// the user acts.
    pub(in crate::daemon::links) fn place_account(&self, place: &Place) -> Account {
        match place {
            Place::Checkout(_) => self.user_account(),
            Place::Space(_) => self.agent_account(),
        }
    }
}
