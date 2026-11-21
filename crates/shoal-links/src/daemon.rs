//! What links need of the rest of the daemon, which implements it: the
//! registered repositories and the spaces requests come from, the watches
//! of their linked items, the messages agents receive beside them, the
//! hub their activity goes up through, which forge
//! each host serves, the permission a scoped caller acts with, the
//! coordinators alerts about a space reach, and the spaces' state links
//! check in their own transactions.
use anyhow::Result;
use futures_util::future::BoxFuture;
use rusqlite::Connection;
use shoal_core::config::Effective;
use shoal_daemon_base::{
    alerts::{Alert, Unread},
    callers::Caller,
    notifications::Coordinators,
    permitted_actions::Action,
};
use shoal_forge_types::{
    ForgeRepo,
    items::{
        link::{ItemKind, Selection},
        reference::{ItemInput, ItemRef},
    },
};
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{
    model::{ConflictCheck, LinkedItem, Repository, Selected, Space, SpaceRef, Workspace},
    processes::Leftover,
    space_names::SpaceSelector,
};
use tokio::sync::watch;

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

    /// The settings of `space`, in its own repository or, for a chat
    /// without one, under the global config.
    fn space_settings<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<Effective>>;

    /// The settings of `workspace`, read from its worktree.
    fn workspace_settings<'a>(
        &'a self,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<Effective>>;

    /// What the daemon records of the agent of the space `id`.
    fn space_agent<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<SpaceAgent>>;

    /// Run the cleanup sweep now, as after a change to what keeps a space.
    fn wake_cleanup(&self);

    /// The registered repository `named` names, as OWNER/NAME on its forge
    /// or by URL; none for another repository or without a name.
    fn registered_named<'a>(
        &'a self,
        named: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Option<Repository>>>;

    /// The spaces whose agent the daemon tracks.
    fn tracked_agents(&self) -> BoxFuture<'_, Vec<SpaceId>>;

    /// Every space present on this machine.
    fn present_spaces(&self) -> BoxFuture<'_, Result<Vec<Space>>>;

    /// The conflicts a workspace watch reports for the workspace
    /// `selector` names, or none when `only_sensitive` and its repository
    /// lists no sensitive files.
    fn watched_conflicts<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        only_sensitive: bool,
    ) -> BoxFuture<'a, Result<Option<ConflictCheck>>>;
}

/// The CI runs spaces link, which the daemon reads until they move.
pub trait Runs: Send + Sync {
    /// The CI run `input` names, by number in the repository `space` acts
    /// in or by URL, as `caller` may link it, with its workflow as its
    /// title.
    fn linked_run<'a>(
        &'a self,
        space: &'a LinkSpace,
        input: &'a ItemInput,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<LinkedItem>>;
}

/// What the daemon wakes for the watches of a space's linked items.
pub trait Watches: Send + Sync {
    /// Wake the agents whose watches just queued updates.
    fn item_updates_queued(&self);

    /// A receiver that changes whenever watches queue updates.
    fn watch_item_updates(&self) -> watch::Receiver<u64>;

    /// Tell those waiting on agents that one's deliveries changed.
    fn agent_changed(&self);

    /// A poll saw an item of the repository `id` merge.
    fn item_merged(&self, id: &RepositoryId);

    /// A poll saw a PR the space `id` links move into or out of draft.
    fn draft_changed(&self, id: &SpaceId);
}

/// What an agent receives beside its items' activity, which the daemon
/// keeps: its messages and the leftover processes of its space.
pub trait Messages: Send + Sync {
    /// The unread messages of the space `id`, oldest first, and whether one
    /// of them wakes an idle agent.
    fn unread_messages<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Unread>>;

    /// The unread messages of `space` that have settled: all of them, or
    /// none while they wait for more.
    fn deliverable_messages<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<Vec<Alert>>>;

    /// The unread `messages` of `space` once they have settled, waiting for
    /// them to.
    fn settled_messages<'a>(
        &'a self,
        space: &'a Space,
        messages: Vec<Alert>,
    ) -> BoxFuture<'a, Result<Vec<Alert>>>;

    /// The leftovers of the space `id` its agent has not been told about,
    /// as many as one delivery names in `room` bytes.
    fn undelivered_leftovers(&self, id: &SpaceId, room: usize) -> Result<Vec<Leftover>>;

    /// A receiver that changes whenever a message is queued for an agent.
    fn watch_messages(&self) -> watch::Receiver<u64>;
}

/// The hub, through which the item activity of this machine's spaces goes
/// up to their coordinators on other machines.
pub trait Elsewhere: Send + Sync {
    /// Whether this machine is the hub or reaches it now.
    fn reaches_hub(&self) -> bool;

    /// Whether a space above `below`, a space directly below one on
    /// another machine, has an agent there.
    fn coordinator_elsewhere<'a>(&'a self, below: &'a SpaceId) -> BoxFuture<'a, bool>;

    /// Pass `message`, the item activity of `space`, up to the space
    /// `above` on another machine, returning once the hub took it.
    fn pass_up_activity<'a>(
        &'a self,
        above: SpaceId,
        space: &'a SpaceRef,
        message: String,
    ) -> BoxFuture<'a, Result<()>>;
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
pub trait Daemon:
    Spaces + Watches + Messages + Elsewhere + Runs + Forges + Permissions + Coordinators
{
}

impl<T> Daemon for T where
    T: Spaces + Watches + Messages + Elsewhere + Runs + Forges + Permissions + Coordinators
{
}

/// What links check of a space in their own transactions. Its methods run
/// on the store's thread, so they are not futures.
pub trait Records: Send + Sync {
    /// Refuse a change to the space `id`'s links while it takes none, as
    /// while a workspace is removed.
    fn ensure_accepts_links(&self, db: &Connection, id: &SpaceId) -> Result<()>;

    /// Whether the space `id` still exists and takes link changes now.
    fn accepts_links(&self, db: &Connection, id: &SpaceId) -> Result<bool>;

    /// Forget the ready marks the space `id` holds on the items at `urls`,
    /// which it no longer links.
    fn forget_ready_marks(&self, db: &Connection, id: &SpaceId, urls: &[String]) -> Result<()>;

    /// Refuse a change to the links, cursor or marks of the space `id`
    /// while it is not ready, as a removed space is not.
    fn require_ready_space(&self, db: &Connection, id: &SpaceId) -> Result<()>;

    /// The spaces below `id` whose activity its agent receives, where
    /// `agents` are the spaces whose agent the daemon tracks.
    fn delegated(&self, db: &Connection, id: &SpaceId, agents: &[SpaceId])
    -> Result<Vec<SpaceRef>>;

    /// The space `id` and those below it whose activity its agent
    /// receives, each as the path down to it from the topmost space that
    /// receives `id`'s own activity.
    fn receiving_paths(
        &self,
        db: &Connection,
        id: &SpaceId,
        agents: &[SpaceId],
    ) -> Result<Vec<Vec<SpaceId>>>;

    /// This machine's spaces whose activity goes to a coordinator on
    /// another machine, by the space above them there: each space without
    /// an agent whose parent runs elsewhere, then those below it whose
    /// activity it would receive, where `agents` are the spaces whose agent
    /// the daemon tracks.
    fn delegated_elsewhere(
        &self,
        db: &Connection,
        agents: &[SpaceId],
    ) -> Result<Vec<(SpaceId, Vec<SpaceRef>)>>;

    /// The branch of the workspace `id` while its conflicts are checked:
    /// none for a chat, a workspace at a commit and one whose assignment is
    /// done.
    fn checked_branch(&self, db: &Connection, id: &SpaceId) -> Result<Option<String>>;
}
