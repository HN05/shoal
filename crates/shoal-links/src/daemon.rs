//! What links need of the rest of the daemon, which implements it: the
//! registered repositories, the spaces requests come from and the
//! workspaces a scope reaches, the watches
//! of their linked items, the messages agents receive beside them, the
//! hub their activity goes up through, which forge
//! each host serves, the permission a scoped caller acts with, the
//! coordinators alerts about a space reach, and the spaces' state links
//! check in their own transactions.
use std::collections::BTreeMap;

use anyhow::Result;
use futures_util::future::BoxFuture;
use rusqlite::{Connection, Transaction};
use shoal_core::config::Effective;
use shoal_daemon_base::{
    alerts::{About, Alert, AlertKind, Unread},
    callers::Caller,
    item_numbers::ItemNumber,
    notifications::Coordinators,
    permitted_actions::Action,
};
use shoal_forge::ci::workflow::WorkflowLink;
use shoal_forge_types::{
    ForgeRepo,
    items::{
        link::{ItemKind, Selection},
        reference::ItemRef,
    },
    numbers::PullNumber,
};
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{
    agent::Agent,
    approvals::ApprovalRequest,
    config::permissions::{self, PermissionKey},
    model::{
        ChangedOnBothSides, ConflictCheck, EventCause, Repository, Selected, Space, SpaceRef,
        Workspace,
    },
    processes::Leftover,
    protocol::operations::OperationSpec,
    space_names::SpaceSelector,
};
use shoal_refs::{BranchName, CommitId};
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

    /// The settings of the registered repository `id`, with the file in
    /// its checkout.
    fn repository_settings<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<Effective>>;

    /// The settings of a place outside a worktree in the repository `id`,
    /// or the global config's alone without one.
    fn settings_in<'a>(&'a self, id: Option<&'a RepositoryId>) -> BoxFuture<'a, Result<Effective>>;

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

    /// The workspaces of the registered repository `id`.
    fn repository_workspaces<'a>(
        &'a self,
        id: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<Vec<Workspace>>>;

    /// The workspaces `caller`'s scope reaches.
    fn reached_workspaces<'a>(
        &'a self,
        caller: &'a Caller,
    ) -> BoxFuture<'a, Result<Vec<Workspace>>>;

    /// Whether `caller`'s scope reaches the space `id`.
    fn reaches<'a>(&'a self, caller: &'a Caller, id: &'a SpaceId) -> BoxFuture<'a, Result<bool>>;

    /// The workspace `selector` names, refusing a chat.
    fn workspace<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Workspace>>;

    /// The workspace `id`, as it is now.
    fn workspace_by_id<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Workspace>>;

    /// The workspace `id`, while it exists.
    fn find_workspace<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<Workspace>>>;

    /// Prove that the worktree of `workspace` is still its own, before a
    /// request relies on it.
    fn verify_worktree<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, Result<()>>;

    /// The registered repositories.
    fn repositories(&self) -> BoxFuture<'_, Result<Vec<Repository>>>;

    /// The workspace of the repository `id` that owns `branch`.
    fn branch_owner<'a>(
        &'a self,
        id: &'a RepositoryId,
        branch: &'a BranchName,
    ) -> BoxFuture<'a, Result<Option<SpaceId>>>;

    /// The workspace of the repository `id` at a commit made for, or
    /// linked to, the fork's PR `url` numbered `number`.
    fn pull_workspace<'a>(
        &'a self,
        id: &'a RepositoryId,
        url: &'a ItemRef,
        number: PullNumber,
    ) -> BoxFuture<'a, Result<Option<Workspace>>>;

    /// The agent whose execution a request from `caller` runs in, which
    /// proposes for it; none for one from outside an agent's execution.
    fn proposing_agent<'a>(&'a self, caller: &'a Caller) -> BoxFuture<'a, Option<Agent>>;

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

/// What the daemon wakes for the watches of a space's linked items.
pub trait Watches: Send + Sync {
    /// Wake the agents whose watches just queued updates.
    fn item_updates_queued(&self);

    /// A receiver that changes whenever watches queue updates.
    fn watch_item_updates(&self) -> watch::Receiver<u64>;

    /// Tell those waiting on agents that one's deliveries changed.
    fn agent_changed(&self);

    /// An item of the repository `id` merged, as a poll or Shoal's own
    /// merge saw.
    fn item_merged(&self, id: &RepositoryId);

    /// A PR the space `id` links moved into or out of draft.
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

    /// Wake the agents a transaction just queued messages for.
    fn message_queued(&self);
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

/// The approval requests that proposals and pushes wait for, which the
/// daemon's approvals subsystem routes and decides, and the operations that
/// run the pushes it approves.
pub trait Approvals: Send + Sync {
    /// Tell whoever decides the pending `request` about it.
    fn announce<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, ()>;

    /// Refuse `request` where its governing permission is `gate` and the
    /// gate agent cannot hear it.
    fn ensure_gate_hears<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, Result<()>>;

    /// Tell the agent of the space `id` how its request went, wherever the
    /// space runs; whether the message was queued.
    fn tell_space<'a>(
        &'a self,
        id: &'a SpaceId,
        kind: AlertKind,
        message: String,
    ) -> BoxFuture<'a, bool>;

    /// Tell the user, or the coordinator of the space `about` names, of a
    /// problem with a request.
    fn notify_about(&self, about: About, kind: AlertKind, message: String) -> BoxFuture<'_, ()>;

    /// Start the operation `spec` describes in `env`, or the daemon's
    /// environment, returning its ID.
    fn start_operation(
        &self,
        spec: OperationSpec,
        env: Option<BTreeMap<String, String>>,
    ) -> BoxFuture<'_, Result<String>>;

    /// Whether the operation `id` still runs.
    fn operation_running(&self, id: &str) -> bool;
}

/// The local reviews a new PR and a PR merge wait for, as `[review]
/// require_for_pr` and `require_for_merge` ask.
pub trait Reviews: Send + Sync {
    /// Whether `settings` require a review of a PR merge from `caller`.
    fn merge_review_required(&self, settings: &Effective, caller: Option<&Caller>) -> bool;

    /// Refuse merging the PR `url`, which `owner` reviews, at `head` for
    /// `caller` until a finished review of `owner` covers it with every
    /// finding resolved, starting one where that review would cover `head`
    /// and `caller`'s scope reaches `owner`.
    fn require_merge_review<'a>(
        &'a self,
        owner: &'a Workspace,
        head: &'a CommitId,
        url: &'a ItemRef,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<()>>;

    /// Whether `settings` require a review of a new PR from `caller`.
    fn pr_review_required(&self, settings: &Effective, caller: Option<&Caller>) -> bool;

    /// Refuse a new PR of `workspace` at `head` from `caller` until a
    /// finished review covers it with every finding resolved, starting a
    /// review where none does.
    fn require_pr_review<'a>(
        &'a self,
        workspace: &'a Workspace,
        head: &'a CommitId,
        caller: Option<&'a Caller>,
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

    /// The permissions of `caller`'s space in `repository`, or under the
    /// global config without one.
    fn caller_permissions<'a>(
        &'a self,
        caller: &'a Caller,
        repository: Option<&'a RepositoryId>,
    ) -> BoxFuture<'a, Result<permissions::Permissions>>;

    /// Let `caller` take each of `actions` together: none is taken until
    /// all may be.
    fn authorize_actions<'a>(
        &'a self,
        caller: Option<&'a Caller>,
        actions: Vec<Action>,
    ) -> BoxFuture<'a, Result<()>>;

    /// Refuse `caller` the actions `key` governs in `repository` where its
    /// space's permission for it is `deny`, before the work that names one.
    fn refuse_denied<'a>(
        &'a self,
        caller: &'a Caller,
        key: PermissionKey,
        repository: Option<&'a RepositoryId>,
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

    /// The repository the workflow URL `url` names, with the workflow and
    /// the branch it names.
    fn resolve_workflow<'a>(
        &'a self,
        url: &'a str,
    ) -> BoxFuture<'a, Result<(ForgeRepo, WorkflowLink)>>;

    /// The forge the repository of `item` is on.
    fn item_forge<'a>(&'a self, item: &'a ItemRef) -> BoxFuture<'a, Result<ForgeRepo>>;
}

/// Everything links need of the daemon, which each of their operations
/// that reaches beyond them takes.
pub trait Daemon:
    Spaces + Watches + Messages + Elsewhere + Approvals + Reviews + Forges + Permissions + Coordinators
{
}

impl<T> Daemon for T where
    T: Spaces
        + Watches
        + Messages
        + Elsewhere
        + Approvals
        + Reviews
        + Forges
        + Permissions
        + Coordinators
{
}

/// Which kind of proposal a request records.
#[derive(Clone, Copy)]
pub enum ProposalKind {
    Issue,
    Pull,
    Push,
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

    /// Each workspace's last conflict check that lists files changed on
    /// both sides.
    fn changed_on_both_sides(&self, db: &Connection) -> Result<Vec<(SpaceId, ChangedOnBothSides)>>;

    /// Record `request` as the proposal of `kind`, or find the identical
    /// one still pending, returning it and whether this recorded it.
    fn record_proposal(
        &self,
        tx: &Transaction<'_>,
        request: ApprovalRequest,
        kind: ProposalKind,
    ) -> Result<(ApprovalRequest, bool)>;

    /// Store the changes made to the pending proposal `request`.
    fn save_proposal(&self, tx: &Transaction<'_>, request: &ApprovalRequest) -> Result<()>;

    /// Journal the decided proposal `request` and forget it.
    fn forget_proposal(&self, tx: &Transaction<'_>, request: &ApprovalRequest) -> Result<()>;

    /// Keep `request` as outdated, so it is no longer decided.
    fn retire_approval(&self, tx: &Transaction<'_>, request: &ApprovalRequest) -> Result<()>;

    /// The approval request numbered `number`, if any.
    fn approval_by_number(
        &self,
        db: &Connection,
        number: ItemNumber,
    ) -> Result<Option<ApprovalRequest>>;

    /// The approval request whose key is `key`, if any.
    fn approval_by_key(&self, db: &Connection, key: &str) -> Result<Option<ApprovalRequest>>;

    /// The approval requests of the space `space`, or of every space.
    fn approval_requests(
        &self,
        db: &Connection,
        space: Option<&SpaceId>,
    ) -> Result<Vec<ApprovalRequest>>;

    /// Whether the workspace `id` is `base` or a workspace whose branch the
    /// branch of `base` builds on.
    fn builds_on(&self, db: &Connection, base: &SpaceId, id: &SpaceId) -> Result<bool>;

    /// Have the workspace `id` build on `new_base`, or the default branch
    /// without one, in place of `base`, journaled for `cause`; false once it
    /// no longer builds on `base`.
    fn change_base(
        &self,
        tx: &Transaction<'_>,
        id: &SpaceId,
        base: &SpaceId,
        new_base: Option<&SpaceId>,
        cause: EventCause,
    ) -> Result<bool>;

    /// Queue Shoal's `message` of `kind` for the agent of the space `id`.
    fn queue_shoal_message(
        &self,
        tx: &Transaction<'_>,
        id: &SpaceId,
        kind: AlertKind,
        message: &str,
    ) -> Result<()>;

    /// The space `id` and every space below it.
    fn subtree(&self, db: &Connection, id: &SpaceId) -> Result<Vec<SpaceId>>;

    /// The branch of the workspace `id` while its conflicts are checked:
    /// none for a chat, a workspace at a commit and one whose assignment is
    /// done.
    fn checked_branch(&self, db: &Connection, id: &SpaceId) -> Result<Option<String>>;
}
