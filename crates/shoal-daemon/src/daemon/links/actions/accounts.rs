//! The forge and the accounts item actions go out with: the user's own
//! logins for reads and the user's decisions, the review account for a
//! scoped caller's verdicts, the agent account for the rest.
use anyhow::{Context, Result};
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};

use crate::{
    config::{
        Config,
        forges::{Forges, Whose},
    },
    daemon::{links::space::LinkSpace, workspace::Manager},
    forge::ForgeRepo,
    model::{Repository, Workspace},
    protocol::ItemContext,
};
use shoal_forge::{RepoRequests, account::Account};

/// Where an action on a repository's items runs: a space, a workspace's
/// worktree verified, acting in the repository whose checkout or worktree's
/// `origin` gives its forge, or the registered checkout of a repository
/// without one.
pub(in crate::daemon::links) enum Place {
    Space(Box<LinkSpace>),
    Checkout(Repository),
}

impl Place {
    /// The workspace the place is, which records the changes it makes.
    pub(in crate::daemon::links) fn workspace(&self) -> Option<Workspace> {
        match self {
            Self::Space(space) => space.to_workspace(),
            Self::Checkout(_) => None,
        }
    }

    /// The repository it acts in.
    pub(in crate::daemon::links) fn repository_id(&self) -> &RepositoryId {
        match self {
            Self::Space(space) => space.repository_id(),
            Self::Checkout(checkout) => &checkout.id,
        }
    }

    /// The workspace or chat, whose linked items an action may default to
    /// and which records the changes it makes.
    pub(in crate::daemon::links) fn space_id(&self) -> Option<&SpaceId> {
        match self {
            Self::Space(space) => Some(&space.id),
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
            .remembering(self.forge_memories.of(&identity))
            .naming_missing(Whose::Agent.missing())
    }

    /// The account scoped callers approve and request changes on PRs as:
    /// the hosts' review tokens, with no other login to fall back on.
    pub(in crate::daemon::links) fn review_account(&self) -> Account {
        let forges = &self.config().machine.forges;
        let identity = format!("review {}", forges.identity(Whose::Review));
        Account::user(forges.logins(Whose::Review))
            .remembering(self.forge_memories.of(&identity))
            .naming_missing(Whose::Review.missing())
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
    pub(crate) async fn checkout_forge(&self, repository: &Repository) -> Result<ForgeRepo> {
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

    /// Where `context` runs, with a workspace's worktree verified. Only a
    /// chat without a repository acts in another's checkout, and only while
    /// it runs.
    pub(in crate::daemon::links) async fn place(&self, context: &ItemContext) -> Result<Place> {
        match context {
            ItemContext::Checkout(repository) => {
                Ok(Place::Checkout(self.repository(repository).await?))
            }
            _ => {
                let space = self.context_space(context).await?;
                self.verify_space(&space).await?;
                anyhow::ensure!(
                    space.keeps_links() || space.running,
                    "no running chat {}",
                    space.name
                );
                Ok(Place::Space(Box::new(space)))
            }
        }
    }

    /// The forge of the place's `origin`, with the agent account acting there.
    pub(in crate::daemon::links) async fn place_forge_account(
        &self,
        place: &Place,
    ) -> Result<(ForgeRepo, Account)> {
        Ok((self.place_forge(place).await?, self.agent_account()))
    }

    /// The forge of the place's `origin`, with the account that changes
    /// items there: the agent account in a space, and the user's own logins
    /// in a registered checkout, where only the user acts.
    pub(in crate::daemon::links) async fn place_change_account(
        &self,
        place: &Place,
    ) -> Result<(ForgeRepo, Account)> {
        match place {
            Place::Checkout(checkout) => {
                Ok((self.checkout_forge(checkout).await?, self.user_account()))
            }
            Place::Space(_) => self.place_forge_account(place).await,
        }
    }

    /// The forge of the place's `origin`.
    pub(in crate::daemon::links) async fn place_forge(&self, place: &Place) -> Result<ForgeRepo> {
        match place {
            Place::Space(space) => self.space_forge(space).await,
            Place::Checkout(checkout) => self.checkout_forge(checkout).await,
        }
    }
}
