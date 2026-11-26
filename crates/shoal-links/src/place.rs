//! Where a request about issues, PRs and CI runs comes from: a space or a
//! registered checkout.
use shoal_daemon_base::forge_access::ForgeAccess;
use std::path::Path;

use anyhow::Result;
use shoal_forge_types::ForgeRepo;
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{
    model::{Repository, Workspace},
    protocol::ItemContext,
};

use crate::{Links, daemon::Daemon, space::LinkSpace};

/// Whether a request only reads what it acts on or changes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    Change,
}

/// Where a request about a repository's items runs: a space, acting in the
/// repository whose checkout or worktree's `origin` gives its forge, or the
/// registered checkout of a repository without one.
pub enum Place {
    Space(Box<LinkSpace>),
    Checkout(Repository),
}

impl Place {
    /// The workspace the place is, which records the changes it makes.
    pub fn workspace(&self) -> Option<Workspace> {
        match self {
            Self::Space(space) => space.to_workspace(),
            Self::Checkout(_) => None,
        }
    }

    /// The repository it acts in.
    pub fn repository_id(&self) -> &RepositoryId {
        match self {
            Self::Space(space) => space.repository_id(),
            Self::Checkout(checkout) => &checkout.id,
        }
    }

    /// The directory whose `origin` names the place's repository.
    pub fn origin_path(&self) -> &Path {
        match self {
            Self::Space(space) => space.origin_path(),
            Self::Checkout(checkout) => &checkout.path,
        }
    }

    /// The workspace or chat the place is.
    pub fn space(&self) -> Option<&LinkSpace> {
        match self {
            Self::Space(space) => Some(space),
            Self::Checkout(_) => None,
        }
    }

    /// The forge of the place's `origin`.
    pub async fn forge(&self, forges: &ForgeAccess) -> Result<ForgeRepo> {
        match self {
            Self::Space(space) => crate::forges::of_space(forges, space).await,
            Self::Checkout(checkout) => forges.checkout_forge(checkout).await,
        }
    }

    /// The workspace or chat, whose linked items a request may default to
    /// and which records the changes it makes.
    pub fn space_id(&self) -> Option<&SpaceId> {
        match self {
            Self::Space(space) => Some(&space.id),
            Self::Checkout(_) => None,
        }
    }
}

impl Links {
    /// Where `context` runs, with a workspace's worktree verified. Only a
    /// chat without a repository acts in another's checkout, and only while
    /// it runs, though a stopped chat still reads its links in its
    /// repository's checkout.
    pub async fn place(
        &self,
        daemon: &dyn Daemon,
        context: &ItemContext,
        access: Access,
    ) -> Result<Place> {
        match context {
            ItemContext::Checkout(repository) => {
                Ok(Place::Checkout(daemon.repository(repository).await?))
            }
            ItemContext::Space { .. } => {
                let space = self.context_space(daemon, context).await?;
                daemon.verify_space(&space).await?;
                anyhow::ensure!(
                    access == Access::Read || space.keeps_links() || space.running,
                    "no running chat {}",
                    space.name
                );
                Ok(Place::Space(Box::new(space)))
            }
        }
    }
}
