//! The space whose items a request links, watches or shows.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{
    model::{Repository, Space, Workspace},
    protocol::ItemContext,
    space_names::SpaceSelector,
};
use shoal_refs::BranchName;

use crate::{Links, daemon::Daemon};

/// A space as a request about its items sees it.
pub struct LinkSpace {
    pub space: Space,
    /// The repository it acts in: its own, or the registered one a chat
    /// without one names, which counts as its own.
    repository: RepositoryId,
    /// The checkout of that registered repository, where a chat without a
    /// repository of its own acts.
    acting_in: Option<PathBuf>,
    /// The registered checkout whose `origin` a workspace whose worktree is
    /// still to be created reads in its place.
    origin_in: Option<PathBuf>,
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
            origin_in: None,
        }
    }
}

impl LinkSpace {
    /// `space` acting in its own repository, which may have been removed
    /// meanwhile; a chat without one is refused.
    pub fn own(space: Space) -> Result<Self> {
        let repository =
            (space.repository_id.clone()).with_context(|| no_own_repository(&space.name))?;
        Ok(Self {
            space,
            repository,
            acting_in: None,
            origin_in: None,
        })
    }

    /// The space without a repository `space`, a chat, acting in the
    /// registered `checkout`, which counts as its own.
    pub fn acting_in(space: Space, checkout: Repository) -> Self {
        Self {
            space,
            repository: checkout.id,
            acting_in: Some(checkout.path),
            origin_in: None,
        }
    }

    /// The space, whose worktree is still to be created, reading its
    /// `origin` in the registered `checkout` meanwhile.
    pub fn origin_in(self, checkout: PathBuf) -> Self {
        Self {
            origin_in: Some(checkout),
            ..self
        }
    }

    pub fn repository_id(&self) -> &RepositoryId {
        &self.repository
    }

    /// Whether the space records the items it opens and links: a chat
    /// without a repository acts in many and records none.
    pub fn keeps_links(&self) -> bool {
        self.space.repository_id.is_some()
    }

    /// The branch whose merge conflicts a workspace's watch reports; a chat
    /// works on none.
    pub fn branch(&self) -> Option<&str> {
        (self.worktree()).and_then(|worktree| worktree.branch().map(BranchName::as_str))
    }

    /// Where an item request of the space runs: the space, in the
    /// repository it acts in when that is not its own.
    pub fn item_context(&self) -> ItemContext {
        ItemContext::Space {
            space: SpaceSelector::id(&self.id),
            repository: (self.acting_in.as_ref()).map(|_| self.repository.clone()),
        }
    }

    /// The directory whose `origin` names the space's repository on its
    /// forge: where it works, or the checkout a chat without a repository
    /// acts in, or a workspace without its worktree yet reads.
    pub fn origin_path(&self) -> &Path {
        (self.acting_in.as_deref())
            .or(self.origin_in.as_deref())
            .unwrap_or_else(|| self.workdir.path())
    }
}

impl Links {
    /// The workspace or running chat `selector` names, by ID or name.
    pub async fn link_space(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
    ) -> Result<LinkSpace> {
        self.own_link_space(daemon, daemon.space(selector).await?)
            .await
    }

    /// `space` acting in its own repository, reading its `origin` in the
    /// registered checkout while its worktree is still to be created.
    pub async fn own_link_space(&self, daemon: &dyn Daemon, space: Space) -> Result<LinkSpace> {
        let pending = (space.worktree()).is_some_and(|worktree| worktree.checkout.is_pending());
        let linked = LinkSpace::own(space)?;
        if !pending {
            return Ok(linked);
        }
        let checkout = daemon.repository(linked.repository_id()).await?.path;
        Ok(linked.origin_in(checkout))
    }

    /// The space `context` names, a workspace or a chat, acting in its own
    /// repository or, for a chat without one, in the registered repository
    /// it names; a registered checkout is none.
    pub async fn context_space(
        &self,
        daemon: &dyn Daemon,
        context: &ItemContext,
    ) -> Result<LinkSpace> {
        match context {
            ItemContext::Space {
                space,
                repository: None,
            } => self.link_space(daemon, space).await,
            ItemContext::Space {
                space,
                repository: Some(repository),
            } => {
                let space = daemon.space(space).await?;
                self.space_acting_in(daemon, space, repository).await
            }
            ItemContext::Checkout(_) => bail!("a registered checkout is no space"),
        }
    }

    /// The space `id`, while it exists, acting in the registered repository
    /// `repository` when it is a chat without a repository.
    pub async fn find_link_space(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        repository: Option<&RepositoryId>,
    ) -> Result<Option<LinkSpace>> {
        let Some(space) = daemon.find_space(id).await? else {
            return Ok(None);
        };
        let outside = space.repository_id.is_none();
        let found = match repository.filter(|_| outside) {
            Some(repository) => self.space_acting_in(daemon, space, repository).await,
            None => self.own_link_space(daemon, space).await,
        };
        found.map(Some)
    }

    /// `space` acting in the registered repository `repository`: its own,
    /// or for a space without one, a chat, any, which then counts as its
    /// own. A space with a repository acts only in that one.
    async fn space_acting_in(
        &self,
        daemon: &dyn Daemon,
        space: Space,
        repository: &RepositoryId,
    ) -> Result<LinkSpace> {
        match &space.repository_id {
            Some(own) => {
                anyhow::ensure!(
                    own == repository,
                    "space {} acts only in its own repository",
                    space.name
                );
                self.own_link_space(daemon, space).await
            }
            None => Ok(LinkSpace::acting_in(
                space,
                daemon.repository(repository).await?,
            )),
        }
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

#[cfg(test)]
mod tests;
