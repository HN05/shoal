//! Where a request about a repository's issues, PRs and CI runs comes from:
//! a space, a workspace's worktree verified, or the registered checkout of
//! a repository, and whether it reads or changes what it acts on there.
use anyhow::Result;
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};

use crate::{
    daemon::{links::space::LinkSpace, workspace::Manager},
    forge::{
        ForgeRepo,
        items::{
            link::ItemKind,
            reference::{ItemInput, ItemRef},
        },
    },
    model::{Repository, Workspace},
    protocol::ItemContext,
};

/// Whether a request only reads what it acts on or changes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::daemon) enum Access {
    Read,
    Change,
}

/// Where a request about a repository's items runs: a space, acting in the
/// repository whose checkout or worktree's `origin` gives its forge, or the
/// registered checkout of a repository without one.
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

    /// The workspace or chat, whose linked items a request may default to
    /// and which records the changes it makes.
    pub(in crate::daemon::links) fn space_id(&self) -> Option<&SpaceId> {
        match self {
            Self::Space(space) => Some(&space.id),
            Self::Checkout(_) => None,
        }
    }
}

impl Manager {
    /// Where `context` runs, with a workspace's worktree verified. Only a
    /// chat without a repository acts in another's checkout, and only while
    /// it runs, though a stopped chat still reads its links in its
    /// repository's checkout.
    pub(in crate::daemon::links) async fn place(
        &self,
        context: &ItemContext,
        access: Access,
    ) -> Result<Place> {
        match context {
            ItemContext::Checkout(repository) => {
                Ok(Place::Checkout(self.repository(repository).await?))
            }
            ItemContext::Space { .. } => {
                let space = self.context_space(context).await?;
                self.verify_space(&space).await?;
                anyhow::ensure!(
                    access == Access::Read || space.keeps_links() || space.running,
                    "no running chat {}",
                    space.name
                );
                Ok(Place::Space(Box::new(space)))
            }
        }
    }

    /// The forge of the place's `origin`.
    pub(in crate::daemon::links) async fn place_forge(&self, place: &Place) -> Result<ForgeRepo> {
        match place {
            Place::Space(space) => self.space_forge(space).await,
            Place::Checkout(checkout) => self.checkout_forge(checkout).await,
        }
    }

    /// The item `item` names in `forge`, the repository `place` runs in,
    /// or the space's linked one of `kind`.
    pub(in crate::daemon::links) async fn place_item(
        &self,
        item: Option<ItemInput>,
        place: &Place,
        forge: &ForgeRepo,
        kind: ItemKind,
    ) -> Result<ItemRef> {
        self.named_or_linked(item, place.space_id(), forge, kind)
            .await
    }
}
