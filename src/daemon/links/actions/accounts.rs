//! The forge and the accounts item actions go out with: the user's own
//! logins for reads and the user's decisions, the agent account for the rest.
use anyhow::{Context, Result};

use crate::{
    config::{
        Config,
        forges::{Forges, Whose},
    },
    daemon::workspace::Manager,
    forge::{ForgeRepo, account::Account},
    model::{Repository, Workspace},
    protocol::ItemContext,
};

/// Where an action on a repository's items runs: a workspace, its worktree
/// verified, or the registered checkout of a repository without one.
pub(in crate::daemon::links) enum Place {
    Workspace(Box<Workspace>),
    Checkout(Repository),
}

impl Place {
    /// The workspace, whose linked items an action may default to.
    pub(in crate::daemon::links) fn workspace(&self) -> Option<&Workspace> {
        match self {
            Self::Workspace(workspace) => Some(workspace),
            Self::Checkout(_) => None,
        }
    }
}

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
            .remembering(self.forge_memories.of(&user_identity(forges)))
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
            .remembering(self.forge_memories.of(&identity))
    }

    /// The forge of the workspace's `origin`.
    pub(in crate::daemon::links) async fn workspace_forge(
        &self,
        workspace: &Workspace,
    ) -> Result<ForgeRepo> {
        let remote = crate::repository::remote_url_from_path(&workspace.path)
            .await?
            .context("forge actions need an origin remote")?;
        self.forge(&remote).await
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
    pub(in crate::daemon::links) async fn checkout_forge(
        &self,
        repository: &Repository,
    ) -> Result<ForgeRepo> {
        let remote = crate::repository::remote_url_from_path(&repository.path)
            .await?
            .context("forge actions need an origin remote")?;
        self.forge(&remote).await
    }

    /// The forge of the registered checkout's `origin`, with the agent
    /// account.
    pub(in crate::daemon::links) async fn checkout_forge_account(
        &self,
        repository: &Repository,
    ) -> Result<(ForgeRepo, Account)> {
        Ok((self.checkout_forge(repository).await?, self.agent_account()))
    }

    /// Where `context` runs, with the workspace's worktree verified.
    pub(in crate::daemon::links) async fn place(&self, context: &ItemContext) -> Result<Place> {
        Ok(match context {
            ItemContext::Workspace(selector) => {
                let workspace = self.workspace(selector).await?;
                self.verify_worktree(&workspace).await?;
                Place::Workspace(Box::new(workspace))
            }
            ItemContext::Checkout(_) | ItemContext::Chat(_) => {
                Place::Checkout(self.context_checkout(context).await?)
            }
        })
    }

    /// The forge of the place's `origin`, with the agent account acting there.
    pub(in crate::daemon::links) async fn place_forge_account(
        &self,
        place: &Place,
    ) -> Result<(ForgeRepo, Account)> {
        match place {
            Place::Workspace(workspace) => self.workspace_forge_account(workspace).await,
            Place::Checkout(repository) => self.checkout_forge_account(repository).await,
        }
    }

    /// The forge of the place's `origin`.
    pub(in crate::daemon::links) async fn place_forge(&self, place: &Place) -> Result<ForgeRepo> {
        match place {
            Place::Workspace(workspace) => self.workspace_forge(workspace).await,
            Place::Checkout(repository) => self.checkout_forge(repository).await,
        }
    }
}
