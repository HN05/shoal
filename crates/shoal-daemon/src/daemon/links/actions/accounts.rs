//! The forge and the accounts item actions go out with: the user's own
//! logins for reads and the user's decisions, the agent account for the rest.
use crate::client::space_ids::SpaceId;
use anyhow::{Context, Result, bail};

use crate::{
    config::{
        Config,
        forges::{Forges, Whose},
    },
    daemon::workspace::Manager,
    forge::{ForgeRepo, account::Account, items::link::ItemKind},
    model::{Repository, Workspace},
    protocol::ItemContext,
};

/// Where an action on a repository's items runs: a workspace, its worktree
/// verified, a chat in its repository's registered checkout, or the
/// registered checkout of a repository without either.
pub(in crate::daemon::links) enum Place {
    Workspace(Box<Workspace>),
    Chat { id: SpaceId, checkout: Repository },
    Checkout(Repository),
}

impl Place {
    pub(in crate::daemon::links) fn workspace(&self) -> Option<&Workspace> {
        match self {
            Self::Workspace(workspace) => Some(workspace),
            Self::Chat { .. } | Self::Checkout(_) => None,
        }
    }

    /// The workspace or chat, whose linked items an action may default to
    /// and which records the changes it makes.
    pub(in crate::daemon::links) fn space_id(&self) -> Option<&SpaceId> {
        match self {
            Self::Workspace(workspace) => Some(&workspace.id),
            Self::Chat { id, .. } => Some(id),
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

    /// Where `context` runs, with the workspace's worktree verified.
    pub(in crate::daemon::links) async fn place(&self, context: &ItemContext) -> Result<Place> {
        Ok(match context {
            ItemContext::Workspace(selector) => {
                let workspace = self.workspace(selector).await?;
                self.verify_worktree(&workspace).await?;
                Place::Workspace(Box::new(workspace))
            }
            ItemContext::Chat(selector) => {
                let chat = self.running_chat(selector).await?;
                let checkout = self.chat_own_checkout(&chat).await?;
                Place::Chat {
                    id: chat.id,
                    checkout,
                }
            }
            // Only a chat without a repository acts in another's checkout,
            // and only while it runs, as any chat's place.
            ItemContext::ChatIn { chat, repository } => {
                let (chat, checkout) = self.chat_acting_in(chat, repository).await?;
                anyhow::ensure!(chat.restore.is_none(), "no running chat {}", chat.name);
                Place::Chat {
                    id: chat.id,
                    checkout,
                }
            }
            ItemContext::Checkout(_) => Place::Checkout(self.context_checkout(context).await?),
        })
    }

    /// The forge of the place's `origin`, with the agent account acting there.
    pub(in crate::daemon::links) async fn place_forge_account(
        &self,
        place: &Place,
    ) -> Result<(ForgeRepo, Account)> {
        match place {
            Place::Workspace(workspace) => self.workspace_forge_account(workspace).await,
            Place::Chat { checkout, .. } | Place::Checkout(checkout) => {
                self.checkout_forge_account(checkout).await
            }
        }
    }

    /// The forge of the place's `origin`.
    pub(in crate::daemon::links) async fn place_forge(&self, place: &Place) -> Result<ForgeRepo> {
        match place {
            Place::Workspace(workspace) => self.workspace_forge(workspace).await,
            Place::Chat { checkout, .. } | Place::Checkout(checkout) => {
                self.checkout_forge(checkout).await
            }
        }
    }

    /// The `named` item, else the one item of `kind` linked to the place's
    /// space.
    pub(in crate::daemon::links) async fn named_or_linked(
        &self,
        named: Option<String>,
        place: &Place,
        kind: ItemKind,
    ) -> Result<String> {
        match (named, place.space_id()) {
            (Some(named), _) => Ok(named),
            (None, Some(id)) => self.linked_item(id, kind).await,
            (None, None) => bail!(super::NOTHING_LINKED),
        }
    }
}
