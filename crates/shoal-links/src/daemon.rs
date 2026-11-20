//! What links need of the rest of the daemon, which implements it: the
//! registered repositories and the spaces requests come from, and which
//! forge each host serves.
use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_forge_types::{
    ForgeRepo,
    items::{link::ItemKind, reference::ItemRef},
};
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{
    model::{Repository, Space},
    protocol::ItemContext,
    space_names::SpaceSelector,
};

use crate::space::LinkSpace;

/// The registered repositories and the spaces requests about items come
/// from.
pub trait Spaces: Send + Sync {
    /// The registered repository `id`.
    fn repository<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<Repository>>;

    /// The workspace or chat `selector` names, by ID or name.
    fn space<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Space>>;

    /// The space `context` names, acting in its own repository or, for a
    /// chat without one, in the registered repository it names.
    fn context_space<'a>(&'a self, context: &'a ItemContext) -> BoxFuture<'a, Result<LinkSpace>>;

    /// Verify a workspace's worktree before a request relies on it.
    fn verify_space<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<()>>;

    /// The space's assigned issues and PRs of `kind`.
    fn linked_items<'a>(
        &'a self,
        id: &'a SpaceId,
        kind: ItemKind,
    ) -> BoxFuture<'a, Result<Vec<ItemRef>>>;
}

/// The forges repositories are on, by the forge each host serves.
pub trait Forges: Send + Sync {
    /// The repository the remote or web URL `url` names.
    fn resolve_forge<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<ForgeRepo>>;

    /// The repository `named` names, as OWNER/NAME on the forge of `own`
    /// or as a repository URL on any forge.
    fn resolve_named<'a>(
        &'a self,
        own: &'a ForgeRepo,
        named: &'a str,
    ) -> BoxFuture<'a, Result<ForgeRepo>>;

    /// The forge the repository of `item` is on.
    fn item_forge<'a>(&'a self, item: &'a ItemRef) -> BoxFuture<'a, Result<ForgeRepo>>;
}

/// Everything links need of the daemon, which each of their operations
/// that reaches beyond them takes.
pub trait Daemon: Spaces + Forges {}

impl<T> Daemon for T where T: Spaces + Forges {}
