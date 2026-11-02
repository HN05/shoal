//! The space whose items a request links, watches or shows: a workspace, or
//! a running chat, which acts in its repository's registered checkout and
//! links items the same way, without a branch or an assignment that
//! completes.
use std::path::Path;

use anyhow::{Context, Result, bail};
use rusqlite::Connection;

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
            Self::Chat { chat, .. } => &chat.repository_id,
        }
    }

    pub(crate) fn workspace(&self) -> Option<&Workspace> {
        match self {
            Self::Workspace(workspace) => Some(workspace),
            Self::Chat { .. } => None,
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
    /// chat's checkout.
    pub(crate) fn item_context(&self) -> ItemContext {
        match self {
            Self::Workspace(workspace) => ItemContext::Workspace(workspace.id.clone()),
            Self::Chat { chat, .. } => ItemContext::Chat(chat.id.clone()),
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
    pub(crate) async fn link_space(&self, selector: &str) -> Result<LinkSpace> {
        Ok(match self.present_space(selector).await? {
            PresentSpace::Workspace(workspace) => LinkSpace::Workspace(Box::new(workspace)),
            PresentSpace::Chat(chat) => {
                let checkout = self.repository(&chat.repository_id).await?;
                LinkSpace::Chat { chat, checkout }
            }
        })
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
            ItemContext::Chat(selector) => {
                let chat = (self.running_chat(selector).await?)
                    .with_context(|| format!("no running chat {selector}"))?;
                self.repository(&chat.repository_id).await
            }
            ItemContext::Workspace(_) => bail!("a workspace acts in its worktree"),
        }
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
