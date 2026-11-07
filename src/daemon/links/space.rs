//! The space whose items a request links, watches or shows: a workspace, or
//! a running chat, which acts in its repository's registered checkout and
//! links items the same way, without a branch or an assignment that
//! completes.
use std::path::Path;

use anyhow::{Context, Result, bail};
use rusqlite::Connection;

use crate::client::space_names::SpaceSelector;
use crate::{
    config::Effective,
    daemon::{store, workspace::Manager},
    forge::ForgeRepo,
    model::{Chat, PresentSpace, Repository, Workspace},
    protocol::ItemContext,
};

#[allow(
    clippy::large_enum_variant,
    reason = "a lookup's result, read by the request that made it"
)]
pub(crate) enum LinkSpace {
    Workspace(Box<Workspace>),
    Chat { chat: Chat, checkout: Repository },
}

impl From<Workspace> for LinkSpace {
    fn from(workspace: Workspace) -> Self {
        Self::Workspace(Box::new(workspace))
    }
}

impl LinkSpace {
    pub(crate) fn id(&self) -> &str {
        match self {
            Self::Workspace(workspace) => &workspace.id,
            Self::Chat { chat, .. } => &chat.id,
        }
    }

    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Workspace(workspace) => &workspace.name,
            Self::Chat { chat, .. } => &chat.name,
        }
    }

    pub(crate) fn repository_id(&self) -> &str {
        match self {
            Self::Workspace(workspace) => &workspace.repository_id,
            Self::Chat { checkout, .. } => &checkout.id,
        }
    }

    pub(crate) fn workspace(&self) -> Option<&Workspace> {
        match self {
            Self::Workspace(workspace) => Some(workspace),
            Self::Chat { .. } => None,
        }
    }

    /// Whether the space records the items it opens and links: a chat
    /// without a repository acts in many and records none.
    pub(crate) fn keeps_links(&self) -> bool {
        match self {
            Self::Workspace(_) => true,
            Self::Chat { chat, .. } => chat.repository_id.is_some(),
        }
    }

    /// The branch whose merge conflicts a workspace's watch reports; a chat
    /// works on none.
    pub(crate) fn branch(&self) -> Option<&str> {
        self.workspace()
            .and_then(|workspace| workspace.branch.as_deref())
    }

    /// Whether the space recorded its work as done, which ends the activity
    /// of its closed items.
    pub(crate) fn done(&self) -> bool {
        match self {
            Self::Workspace(workspace) => workspace.done,
            Self::Chat { chat, .. } => chat.completion.is_some(),
        }
    }

    /// Where an item request of the space runs: the workspace, or the
    /// chat's checkout, the one it acts in for a chat without a repository.
    pub(crate) fn item_context(&self) -> ItemContext {
        match self {
            Self::Workspace(workspace) => ItemContext::Workspace(SpaceSelector::id(&workspace.id)),
            Self::Chat { chat, checkout } if chat.repository_id.is_none() => ItemContext::ChatIn {
                chat: SpaceSelector::id(&chat.id),
                repository: checkout.id.clone(),
            },
            Self::Chat { chat, .. } => ItemContext::Chat(SpaceSelector::id(&chat.id)),
        }
    }

    /// The issues and PRs linked to the space under any relation.
    pub(crate) fn links(&self) -> &crate::model::SpaceLinks {
        match self {
            Self::Workspace(workspace) => &workspace.links,
            Self::Chat { chat, .. } => &chat.links,
        }
    }

    /// The directory whose `origin` names the space's repository on its
    /// forge: the workspace's worktree, or the chat's checkout.
    pub(crate) fn origin_path(&self) -> &Path {
        match self {
            Self::Workspace(workspace) => &workspace.path,
            Self::Chat { checkout, .. } => &checkout.path,
        }
    }
}

impl Manager {
    /// The workspace or running chat `selector` names, by ID or name.
    pub(crate) async fn link_space(&self, selector: &SpaceSelector) -> Result<LinkSpace> {
        Ok(match self.space(selector).await? {
            PresentSpace::Workspace(workspace) => LinkSpace::Workspace(Box::new(workspace)),
            PresentSpace::Chat(chat) => {
                let checkout = self.chat_own_checkout(&chat).await?;
                LinkSpace::Chat { chat, checkout }
            }
        })
    }

    /// The space `context` names: a workspace or a chat by its selector, or
    /// a chat without a repository acting in the registered repository it
    /// names; a registered checkout is none.
    pub(crate) async fn context_space(&self, context: &ItemContext) -> Result<LinkSpace> {
        match context {
            ItemContext::Workspace(selector) | ItemContext::Chat(selector) => {
                self.link_space(selector).await
            }
            ItemContext::ChatIn { chat, repository } => self.chat_in(chat, repository).await,
            ItemContext::Checkout(_) => bail!("a registered checkout is no space"),
        }
    }

    /// [`Self::context_space`], or none for a registered checkout.
    pub(crate) async fn context_space_if_any(
        &self,
        context: &ItemContext,
    ) -> Result<Option<LinkSpace>> {
        match context {
            ItemContext::Checkout(_) => Ok(None),
            _ => self.context_space(context).await.map(Some),
        }
    }

    /// The chat without a repository `chat` names, acting in the registered
    /// repository `repository` names, which counts as its own.
    async fn chat_in(&self, chat: &SpaceSelector, repository: &str) -> Result<LinkSpace> {
        let (chat, checkout) = self.chat_acting_in(chat, repository).await?;
        Ok(LinkSpace::Chat { chat, checkout })
    }

    /// The chat without a repository `chat` names and the checkout of the
    /// registered repository `repository` names, which it acts in.
    pub(crate) async fn chat_acting_in(
        &self,
        chat: &SpaceSelector,
        repository: &str,
    ) -> Result<(Chat, Repository)> {
        let chat = self.chat(chat).await?;
        anyhow::ensure!(
            chat.repository_id.is_none(),
            "chat {} acts only in its own repository",
            chat.name
        );
        Ok((chat, self.repository(repository).await?))
    }

    /// The space `id`, while it exists, acting in the registered repository
    /// `repository` when it is a chat without a repository.
    pub(crate) async fn find_link_space(
        &self,
        id: &str,
        repository: Option<&str>,
    ) -> Result<Option<LinkSpace>> {
        let owned = id.to_owned();
        let kind = (self.store)
            .run(move |db| crate::daemon::spaces::space_kind(db, &owned))
            .await?;
        if kind.is_none() {
            return Ok(None);
        }
        let outside = (self.chat_by_id(id).await?).is_some_and(|chat| chat.repository_id.is_none());
        let space = SpaceSelector::id(id);
        match repository.filter(|_| outside) {
            Some(repository) => self.chat_in(&space, repository).await.map(Some),
            None => self.link_space(&space).await.map(Some),
        }
    }

    /// Verify a workspace's worktree before a request relies on it; a
    /// chat's checkout is the registered one.
    pub(crate) async fn verify_space(&self, space: &LinkSpace) -> Result<()> {
        match space.workspace() {
            Some(workspace) => self.verify_worktree(workspace).await,
            None => Ok(()),
        }
    }

    /// The registered checkout a request from `context` acts in outside a
    /// workspace: the one it names, or the repository's of the running chat
    /// it names.
    pub(crate) async fn context_checkout(&self, context: &ItemContext) -> Result<Repository> {
        match context {
            ItemContext::Checkout(selector) => self.repository(selector).await,
            ItemContext::ChatIn { chat, repository } => {
                Ok(self.chat_acting_in(chat, repository).await?.1)
            }
            ItemContext::Chat(selector) => {
                let chat = self.running_chat(selector).await?;
                self.chat_own_checkout(&chat).await
            }
            ItemContext::Workspace(_) => bail!("a workspace acts in its worktree"),
        }
    }

    /// The registered repository `named` names, as OWNER/NAME on its forge
    /// or by URL; none for another repository or without a name. A name two
    /// registrations answer to is refused, naming both.
    pub(crate) async fn registered_named(&self, named: Option<&str>) -> Result<Option<Repository>> {
        let Some(named) = named else {
            return Ok(None);
        };
        let mut found = Vec::new();
        for repository in self.repositories().await? {
            if self.answers_to(&repository, named).await? {
                found.push(repository);
            }
        }
        ensure_one(named, &found)?;
        Ok(found.pop())
    }

    /// Whether the registered `repository` is the one `named` names: never
    /// for a checkout without an origin, while any other failure to tell
    /// refuses rather than passing for another repository.
    async fn answers_to(&self, repository: &Repository, named: &str) -> Result<bool> {
        let path = &repository.path;
        let remote = self
            .check_access(path, crate::repository::remote_url_from_path(path))
            .await?;
        let Some(remote) = remote else {
            return Ok(false);
        };
        let matches = async {
            let own = self.forge(&remote).await?;
            anyhow::Ok(own.resolve_named(named, &self.forge_hosts()).await? == own)
        };
        (matches.await).with_context(|| {
            let name = crate::repository::name(repository);
            format!("cannot tell whether {named} is the registered repository {name}")
        })
    }

    /// The registered checkout of `chat`'s repository, where its item
    /// requests act; a chat without a repository has none.
    pub(crate) async fn chat_own_checkout(&self, chat: &Chat) -> Result<Repository> {
        (self.chat_repository(chat).await?).with_context(|| {
            format!(
                "chat {} has no repository of its own; name the repository with --repo or an \
                 issue or PR URL",
                chat.name
            )
        })
    }

    /// The settings of the space: a workspace's own, or a chat's
    /// repository's.
    pub(crate) async fn space_settings(&self, space: &LinkSpace) -> Result<Effective> {
        match space {
            LinkSpace::Workspace(workspace) => self.workspace_settings(workspace).await,
            LinkSpace::Chat { checkout, .. } => self.repository_settings(&checkout.id).await,
        }
    }

    /// The forge of the space's `origin`.
    pub(crate) async fn space_forge(&self, space: &LinkSpace) -> Result<ForgeRepo> {
        let remote = crate::repository::remote_url_from_path(space.origin_path())
            .await?
            .context("forge actions need an origin remote")?;
        self.forge(&remote).await
    }
}

/// Refuse a name `found` lists several registered repositories for,
/// naming each.
fn ensure_one(named: &str, found: &[Repository]) -> Result<()> {
    if found.len() > 1 {
        let names: Vec<String> = (found.iter())
            .map(|repo| format!("{} ({})", crate::repository::name(repo), repo.id))
            .collect();
        bail!(
            "{named} names several registered repositories: {}; name one by its URL",
            names.join(", ")
        );
    }
    Ok(())
}

/// Whether `id` names a recorded chat, whose links change whenever it runs.
pub(crate) fn is_chat(db: &Connection, id: &str) -> Result<bool> {
    store::exists(db, "SELECT 1 FROM chats WHERE id=?1", [id])
}

/// Refuse a change to the links, cursor or marks of a workspace that is not
/// ready; a chat takes one whenever it is recorded.
pub(crate) fn require_ready_space(db: &Connection, id: &str) -> Result<()> {
    if is_chat(db, id)? {
        return Ok(());
    }
    store::require_ready(db, id)
}

#[cfg(test)]
mod tests;
