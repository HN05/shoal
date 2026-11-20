//! What links need of the rest of the daemon, which implements it: the
//! registered repositories and the spaces requests come from, which forge
//! each host serves, the permission a scoped caller acts with, the
//! coordinators alerts about a space reach, and the spaces' state links
//! check in their own transactions.
use anyhow::Result;
use futures_util::future::BoxFuture;
use rusqlite::Connection;
use shoal_core::config::Effective;
use shoal_daemon_base::{callers::Caller, notifications::Coordinators, permitted_actions::Action};
use shoal_forge::updates::Edited;
use shoal_forge_types::{
    ForgeRepo,
    items::{
        link::{ItemKind, Selection},
        reference::ItemRef,
    },
};
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{
    model::{Repository, Selected, Space},
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

    /// The workspace or chat with the ID `id`, while it exists.
    fn find_space<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<Space>>>;

    /// Verify a workspace's worktree before a request relies on it.
    fn verify_space<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<()>>;

    /// The space's assigned issues and PRs of `kind`.
    fn linked_items<'a>(
        &'a self,
        id: &'a SpaceId,
        kind: ItemKind,
    ) -> BoxFuture<'a, Result<Vec<ItemRef>>>;

    /// The items `selection` names among the space's links, each with the
    /// relations the space links it under. With `elsewhere`, an issue or
    /// PR URL of another repository selects that item.
    fn select_items<'a>(
        &'a self,
        space: &'a LinkSpace,
        selection: &'a Selection,
        elsewhere: bool,
    ) -> BoxFuture<'a, Result<Selected>>;

    /// The settings of `space` in the repository it acts in.
    fn acting_settings<'a>(&'a self, space: &'a LinkSpace) -> BoxFuture<'a, Result<Effective>>;

    /// What the daemon records of the agent of the space `id`.
    fn space_agent<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<SpaceAgent>>;
}

/// What the daemon records of a space's agent, which settling the agent
/// account on its issues reads.
pub struct SpaceAgent {
    /// Whether the space's work is done.
    pub done: bool,
    /// Whether it has an agent that runs, sleeps, is queued, restarts or
    /// waits for a conversion to restart it.
    pub present: bool,
}

/// The permission a scoped caller acts with.
pub trait Permissions: Send + Sync {
    /// Let `caller` take `action` as its space's permission for the
    /// action's key allows; the user takes every action.
    fn authorize_action<'a>(
        &'a self,
        caller: Option<&'a Caller>,
        action: Action,
    ) -> BoxFuture<'a, Result<()>>;
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
pub trait Daemon: Spaces + Forges + Permissions + Coordinators {}

impl<T> Daemon for T where T: Spaces + Forges + Permissions + Coordinators {}

/// What links check of a space in their own transactions. Its methods run
/// on the store's thread, so they are not futures.
pub trait Records: Send + Sync {
    /// Refuse a change to the space `id`'s links while it takes none, as
    /// while a workspace is removed.
    fn ensure_accepts_links(&self, db: &Connection, id: &SpaceId) -> Result<()>;

    /// Forget the ready marks the space `id` holds on the items at `urls`,
    /// which it no longer links.
    fn forget_ready_marks(&self, db: &Connection, id: &SpaceId, urls: &[String]) -> Result<()>;

    /// Refuse a change to the links, cursor or marks of the space `id`
    /// while it is not ready, as a removed space is not.
    fn require_ready_space(&self, db: &Connection, id: &SpaceId) -> Result<()>;

    /// Record `edited`, a change Shoal just made on its own to the item
    /// `url`, on every space's cursor of the item, so no watch reports it.
    fn record_shoal_change(&self, db: &Connection, url: &str, edited: &Edited) -> Result<()>;
}
