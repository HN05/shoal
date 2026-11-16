//! The space whose items a request links, watches or shows: a workspace, or
//! a running chat, which acts in its repository's registered checkout and
//! links items the same way, without a branch or an assignment that
//! completes.
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use shoal_refs::BranchName;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rusqlite::Connection;

use crate::{
    config::Effective,
    daemon::{store, workspace::Manager},
    forge::ForgeRepo,
    model::{Repository, Space, Workspace},
    protocol::ItemContext,
};
use shoal_forge::RepoRequests;
use shoal_protocol::space_names::SpaceSelector;

/// A space as a request about its items sees it.
pub struct LinkSpace {
    pub space: Space,
    /// The repository it acts in: its own, or the registered one a chat
    /// without one names, which counts as its own.
    repository: RepositoryId,
    /// The checkout of that registered repository, where a chat without a
    /// repository of its own acts.
    acting_in: Option<PathBuf>,
}

impl std::ops::Deref for LinkSpace {
    type Target = Space;

    fn deref(&self) -> &Space {
        &self.space
    }
}

impl From<Workspace> for LinkSpace {
    fn from(workspace: Workspace) -> Self {
        let repository = workspace.repository().clone();
        Self {
            space: workspace.into(),
            repository,
            acting_in: None,
        }
    }
}

impl LinkSpace {
    /// `space` acting in its own repository, which may have been removed
    /// meanwhile; a chat without one is refused.
    pub(crate) fn own(space: Space) -> Result<Self> {
        let repository =
            (space.repository_id.clone()).with_context(|| no_own_repository(&space.name))?;
        Ok(Self {
            space,
            repository,
            acting_in: None,
        })
    }

    /// The space without a repository `space`, a chat, acting in the
    /// registered `checkout`, which counts as its own.
    pub(crate) fn acting_in(space: Space, checkout: Repository) -> Self {
        Self {
            space,
            repository: checkout.id,
            acting_in: Some(checkout.path),
        }
    }

    pub(crate) fn repository_id(&self) -> &RepositoryId {
        &self.repository
    }

    /// Whether the space records the items it opens and links: a chat
    /// without a repository acts in many and records none.
    pub(crate) fn keeps_links(&self) -> bool {
        self.space.repository_id.is_some()
    }

    /// The branch whose merge conflicts a workspace's watch reports; a chat
    /// works on none.
    pub(crate) fn branch(&self) -> Option<&str> {
        (self.worktree()).and_then(|worktree| worktree.branch().map(BranchName::as_str))
    }

    /// Where an item request of the space runs: the space, in the
    /// repository it acts in when that is not its own.
    pub(crate) fn item_context(&self) -> ItemContext {
        ItemContext::Space {
            space: SpaceSelector::id(&self.id),
            repository: (self.acting_in.as_ref()).map(|_| self.repository.clone()),
        }
    }

    /// The directory whose `origin` names the space's repository on its
    /// forge: where it works, or the checkout a chat without a repository
    /// acts in.
    pub(crate) fn origin_path(&self) -> &Path {
        (self.acting_in.as_deref()).unwrap_or_else(|| self.workdir.path())
    }
}

impl Manager {
    /// The workspace or running chat `selector` names, by ID or name.
    pub async fn link_space(&self, selector: &SpaceSelector) -> Result<LinkSpace> {
        LinkSpace::own(self.space(selector).await?)
    }

    /// The space `context` names, a workspace or a chat, acting in its own
    /// repository or, for a chat without one, in the registered repository
    /// it names; a registered checkout is none.
    pub(crate) async fn context_space(&self, context: &ItemContext) -> Result<LinkSpace> {
        match context {
            ItemContext::Space {
                space,
                repository: None,
            } => self.link_space(space).await,
            ItemContext::Space {
                space,
                repository: Some(repository),
            } => self.space_acting_in(space, repository).await,
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
            ItemContext::Space { .. } => self.context_space(context).await.map(Some),
        }
    }

    /// The space `space` names acting in the registered repository
    /// `repository` names: its own, or for a space without one, a chat, any,
    /// which then counts as its own. A space with a repository acts only in
    /// that one.
    async fn space_acting_in(
        &self,
        space: &SpaceSelector,
        repository: &RepositoryId,
    ) -> Result<LinkSpace> {
        let space = self.space(space).await?;
        match &space.repository_id {
            Some(own) => {
                anyhow::ensure!(
                    own == repository,
                    "space {} acts only in its own repository",
                    space.name
                );
                LinkSpace::own(space)
            }
            None => Ok(LinkSpace::acting_in(
                space,
                self.repository(repository).await?,
            )),
        }
    }

    /// The space `id`, while it exists, acting in the registered repository
    /// `repository` when it is a chat without a repository.
    pub(crate) async fn find_link_space(
        &self,
        id: &SpaceId,
        repository: Option<&RepositoryId>,
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
            Some(repository) => self.space_acting_in(&space, repository).await.map(Some),
            None => self.link_space(&space).await.map(Some),
        }
    }

    /// Verify a workspace's worktree before a request relies on it; a
    /// chat owns no worktree to verify.
    pub(crate) async fn verify_space(&self, space: &Space) -> Result<()> {
        match space.to_workspace() {
            Some(workspace) => self.verify_worktree(&workspace).await,
            None => Ok(()),
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

    /// The settings of the space: a workspace's own, or a chat's
    /// repository's.
    pub(crate) async fn space_settings(&self, space: &LinkSpace) -> Result<Effective> {
        match space.to_workspace() {
            Some(workspace) => self.workspace_settings(&workspace).await,
            None => self.repository_settings(space.repository_id()).await,
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

/// The refusal of a request that needs the repository of the chat `name`,
/// which has none of its own.
fn no_own_repository(name: &shoal_protocol::space_names::SpaceName) -> String {
    format!(
        "chat {name} has no repository of its own; name the repository with --repo or an issue \
         or PR URL"
    )
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
pub(crate) fn is_chat(db: &Connection, id: &SpaceId) -> Result<bool> {
    store::exists(db, "SELECT 1 FROM spaces WHERE id=?1 AND kind='chat'", [id])
}

/// Refuse a change to the links, cursor or marks of a workspace that is not
/// ready; a chat takes one whenever it is recorded.
pub(crate) fn require_ready_space(db: &Connection, id: &SpaceId) -> Result<()> {
    if is_chat(db, id)? {
        return Ok(());
    }
    store::require_ready(db, id)
}

#[cfg(test)]
mod tests;
