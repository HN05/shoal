//! What reviews and the gate need of the rest of the daemon, which
//! implements it: the workspaces they review, their repositories and
//! settings, the agent queue that admits their runs, the agents and
//! coordinators that hear of them, the forge a PR's review is posted on,
//! and for the gate the spaces whose requests it decides and the approvals
//! that carry out its verdicts.
use shoal_daemon_base::callers::Caller;
use shoal_protocol::hub::Member;
use shoal_protocol::protocol::{Body, Operation};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::Result;
use futures_util::future::BoxFuture;
use rusqlite::{Connection, Transaction};
use shoal_daemon_base::notifications::About;
use shoal_forge::transport::account::Account;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::alerts::AlertKind;
use shoal_protocol::approvals::ApprovalRequest;
use shoal_protocol::code::CodeAt;
use shoal_protocol::config::permissions::Permissions;
use shoal_protocol::item_numbers::ItemNumber;
use shoal_protocol::repository::Selector;
use shoal_protocol::reviews::{ReviewId, Severity};
use shoal_protocol::space_names::{SpaceName, SpaceSelector};
use shoal_protocol::threads::{Thread, ThreadSpace};
use shoal_protocol::views::Viewed;
use shoal_refs::CommitId;

use crate::{
    config::Effective,
    forge::{ForgeRepo, items::reference::ItemRef, items::review::NewReview},
    model::{Repository, Space, Workspace},
};

/// The workspaces reviews are of.
pub trait Spaces: Send + Sync {
    /// The workspace `selector` names, refusing a chat.
    fn workspace<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Workspace>>;

    /// The workspace `id`, as it is now.
    fn workspace_by_id<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Workspace>>;

    /// Prove that the worktree of `workspace` is still its own, before its
    /// configuration and commits are read there.
    fn verify_worktree<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, Result<()>>;

    /// The committed HEAD of `workspace`, refused when its worktree is not
    /// on the branch, or at the commit, it records.
    fn head<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, Result<CommitId>>;

    /// The commit `shoal diff` compares the workspace `id` with.
    fn diff_base<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<CommitId>>;
}

/// The registered repositories reviews run in.
pub trait Repositories: Send + Sync {
    /// The registered repository `id`.
    fn repository<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<Repository>>;

    /// The registered repository `selector` names.
    fn select_repository(&self, selector: Selector) -> BoxFuture<'_, Result<Repository>>;

    /// Every registered repository.
    fn repositories(&self) -> BoxFuture<'_, Result<Vec<Repository>>>;

    /// Where the false positives of the repository `id` are kept, while its
    /// local config has a place.
    fn false_positives_path<'a>(
        &'a self,
        id: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<Option<PathBuf>>>;

    /// [`Self::false_positives_path`] for an entry to add, refusing where
    /// the local config cannot be written. The caller holds the registry
    /// gate.
    fn writable_false_positives_path<'a>(
        &'a self,
        id: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<PathBuf>>;
}

/// The settings reviews run under.
pub trait Settings: Send + Sync {
    /// The settings of `workspace` after every layer.
    fn settings<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, Result<Effective>>;

    /// [`Self::settings`] with the templates that build an agent's
    /// instructions.
    fn settings_with_templates<'a>(
        &'a self,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<Effective>>;

    /// The settings of the repository `id` itself, which no branch's own
    /// config file changes.
    fn repository_settings<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<Effective>>;

    /// The variables `settings` configure for the processes of
    /// `workspace`.
    fn environment<'a>(
        &'a self,
        workspace: &'a Workspace,
        settings: &'a Effective,
    ) -> BoxFuture<'a, Result<BTreeMap<String, OsString>>>;
}

/// The threads of a review's findings, which the daemon follows once their
/// records commit.
pub trait Threads: Send + Sync {
    /// Wake what waits for threads after a review of `workspace` opened,
    /// reopened, delivered or dropped its threads, and keep the versions of
    /// the code they point at.
    fn review_threads_changed<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, ()>;

    /// Post `text` in the thread `thread` as `caller`, as its `shoal send`
    /// does.
    fn post_in_thread<'a>(
        &'a self,
        caller: &'a Caller,
        thread: ItemNumber,
        text: String,
    ) -> BoxFuture<'a, Result<()>>;
}

/// The agent queue that admits a review's run, and the agents that hear
/// of a review.
pub trait Agents: Send + Sync {
    /// Ask for admission in `repository` for a one-shot run serving the
    /// space `space`, which waits in the queue at that space's rank. The
    /// request and the admission end with the returned value. Protection
    /// calls `stop` to stop the run; a run it stopped asks again
    /// `after_stop`.
    fn admit<'a>(
        &'a self,
        repository: &'a RepositoryId,
        space: &'a SpaceId,
        after_stop: bool,
        stop: Box<dyn FnOnce() + Send + Sync>,
    ) -> BoxFuture<'a, Result<Box<dyn Admission + 'a>>>;

    /// Hold an admission in `repository` until the daemon restarts, for a
    /// run a stopped daemon left whose processes could not be proven
    /// stopped.
    fn hold_admission(&self, repository: &RepositoryId);

    /// Queue `message`, an alert of `kind`, for the agents of the space
    /// `id`.
    fn message<'a>(
        &'a self,
        id: &'a SpaceId,
        kind: AlertKind,
        message: String,
    ) -> BoxFuture<'a, Result<()>>;

    /// Tell the coordinator of the space `id`, named `name`, about one of
    /// its reviews or gate decisions, in an alert of `kind`.
    fn tell_coordinator<'a>(
        &'a self,
        name: &'a SpaceName,
        id: &'a SpaceId,
        kind: AlertKind,
        message: String,
    ) -> BoxFuture<'a, ()>;
}

/// A one-shot run's admission in the agent queue, until it is dropped.
pub trait Admission: Send {
    /// Wait until admission lets the run start.
    fn admitted(&mut self) -> BoxFuture<'_, ()>;

    /// Keep the admission of a run whose processes may still be running
    /// until the daemon restarts.
    fn keep(self: Box<Self>);
}

/// The forge a PR's review is read from and posted on.
pub trait Forge: Send + Sync {
    /// The forge of `workspace`'s repository and the account its agents
    /// act as there.
    fn forge_account<'a>(
        &'a self,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<(ForgeRepo, Account)>>;

    /// Post `review` on the PR `pull` as `workspace`'s agents, recording
    /// the change as the space's own.
    fn post<'a>(
        &'a self,
        workspace: &'a Workspace,
        pull: &'a ItemRef,
        review: NewReview,
    ) -> BoxFuture<'a, Result<()>>;
}

/// The other machines of the hub, which keep the reviews of the spaces
/// they run.
pub trait Machines: Send + Sync {
    /// The other machines that run a space `caller` sees; none outside a
    /// hub.
    fn machines_seen<'a>(
        &'a self,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Vec<Member>>>;

    /// What `machine` answers `operation`, sent for `caller`.
    fn ask_machine<'a>(
        &'a self,
        machine: &'a Member,
        operation: Operation,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Body>>;
}

/// Everything reviews need of the daemon, which each of their operations
/// that reaches beyond them takes.
pub trait Daemon: Spaces + Repositories + Settings + Agents + Forge + Threads + Machines {}

impl<T: Spaces + Repositories + Settings + Agents + Forge + Threads + Machines> Daemon for T {}

/// The spaces whose approval requests the gate decides.
pub trait Requesters: Send + Sync {
    /// The space `id`, a workspace or a chat.
    fn space<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Space>>;

    /// The issues and PRs linked to the space `id` as `shoal view` reads
    /// them, without their comments.
    fn view_items<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Viewed>>;

    /// The permissions of the space `id` for an action in `repository`.
    fn permissions<'a>(
        &'a self,
        id: &'a SpaceId,
        repository: Option<&'a RepositoryId>,
    ) -> BoxFuture<'a, Result<Permissions>>;

    /// Tell the agents of the space `id`, wherever it runs, `message`, an
    /// alert of `kind`; whether it was queued for them.
    fn tell_space<'a>(
        &'a self,
        id: &'a SpaceId,
        kind: AlertKind,
        message: String,
    ) -> BoxFuture<'a, bool>;

    /// Tell the user `message`, of `kind`, about what `about` names.
    fn notify(&self, about: About, kind: AlertKind, message: String) -> BoxFuture<'_, ()>;
}

/// The approvals that carry out the gate's verdicts.
pub trait Approvals: Send + Sync {
    /// Approve `allowed`, which the gate allowed, as the user's approval
    /// would.
    fn approve<'a>(&'a self, allowed: &'a ApprovalRequest) -> BoxFuture<'a, Result<()>>;

    /// Tell whoever decides the pending `request` about it.
    fn announce<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, ()>;

    /// Remove the files a denied proposal attached.
    fn prune_proposal_files<'a>(&'a self, denied: &'a ApprovalRequest) -> BoxFuture<'a, ()>;
}

/// Everything the gate needs of the daemon.
pub trait GateDaemon: Daemon + Requesters + Approvals {}

impl<T: Daemon + Requesters + Approvals> GateDaemon for T {}

/// The approval requests the gate decides, which it reads and changes in
/// the transactions that record its verdicts, so a verdict and the
/// request's state change as one. Its methods run on the store's thread,
/// so they are not futures.
pub trait Requests: Send + Sync {
    /// The request stored under `key`.
    fn request(&self, db: &Connection, key: &str) -> Result<Option<ApprovalRequest>>;

    /// Hand the pending, unrouted request stored under `key` to the gate;
    /// false when it is no longer one to hand over.
    fn give(&self, tx: &Transaction<'_>, key: &str) -> Result<bool>;

    /// The pending requests the gate is deciding, oldest first.
    fn deciding(&self, db: &Connection) -> Result<Vec<ApprovalRequest>>;

    /// The pending requests the gate allowed whose approval never started.
    fn stranded(&self, db: &Connection) -> Result<Vec<ApprovalRequest>>;

    /// Record that the gate allowed the request stored under `key`.
    fn allow(&self, tx: &Transaction<'_>, key: &str) -> Result<ApprovalRequest>;

    /// Deny the request stored under `key`, as the gate did.
    fn deny(&self, tx: &Transaction<'_>, key: &str) -> Result<ApprovalRequest>;

    /// Pass the request stored under `key` on, for `reason`, quoting the
    /// case `rule`.
    fn escalate(
        &self,
        tx: &Transaction<'_>,
        key: &str,
        reason: &str,
        rule: &str,
    ) -> Result<ApprovalRequest>;

    /// Give the request stored under `key`, which the gate cannot judge for
    /// `reason`, to the normal route.
    fn unjudged(&self, tx: &Transaction<'_>, key: &str, reason: &str) -> Result<ApprovalRequest>;

    /// Give the allowed request stored under `key`, whose approval failed
    /// with `failure`, to the normal route while it is still pending.
    fn unapplied(
        &self,
        tx: &Transaction<'_>,
        key: &str,
        failure: &str,
    ) -> Result<Option<ApprovalRequest>>;
}

/// A finding's `review` thread to open: the review whose finding it is,
/// the workspace that holds it, how much it matters, its title, which names
/// the thread, its body, the opening post, and the code that post points
/// at in the reviewed commit.
pub struct FindingThread<'a> {
    pub review: ReviewId,
    pub workspace: &'a SpaceId,
    pub severity: Severity,
    pub title: &'a str,
    pub body: &'a str,
    pub code: &'a [FindingCode],
}

/// Where a finding is in the reviewed commit `commit`: its file, lines and
/// the blob of the file there, and the snippet of its lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindingCode {
    pub commit: CommitId,
    pub at: CodeAt,
    pub snippet: Option<String>,
}

/// A holder's post that ends a finding's thread, which waits for the
/// reviewer's answer: the thread's record and the post's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AwaitedPost {
    pub thread: i64,
    pub post: i64,
}

/// The records of the spaces reviews are of and of the threads of their
/// findings, which reviews read and write in the connection or transaction
/// that reads or records the reviews, so a review, its workspace and its
/// threads change as one. Its methods run on the store's thread, so they
/// are not futures.
pub trait Records: Send + Sync {
    /// The name of the space `id`, while it exists.
    fn space_name(&self, db: &Connection, id: &SpaceId) -> Result<Option<SpaceName>>;

    /// The space `id` as threads name it, by its number and name while it
    /// exists.
    fn thread_space(&self, db: &Connection, id: &SpaceId) -> Result<ThreadSpace>;

    /// The workspaces of `repository`, or every workspace without one.
    fn workspaces(
        &self,
        db: &Connection,
        repository: Option<&RepositoryId>,
    ) -> Result<Vec<SpaceId>>;

    /// Open the `review` thread of `finding` from Shoal, held by its
    /// workspace, with its body as the opening post, holding its posts
    /// until its review is submitted; the thread's record ID. It tells no
    /// one: the review's own alert names its threads.
    fn open_finding(&self, tx: &Transaction<'_>, finding: &FindingThread<'_>) -> Result<i64>;

    /// The record ID of the thread `number` when it is a `review` thread
    /// Shoal opened that `workspace` holds resolved `fixed`, which a later
    /// review may reopen.
    fn fixed_finding(
        &self,
        db: &Connection,
        workspace: &SpaceId,
        number: ItemNumber,
    ) -> Result<Option<i64>>;

    /// Reopen the thread `thread` with a post from Shoal saying `why`,
    /// while [`Self::fixed_finding`] still names it; whether it did. Like
    /// opening, it tells no one.
    fn reopen_finding(
        &self,
        tx: &Transaction<'_>,
        workspace: &SpaceId,
        thread: i64,
        why: &str,
    ) -> Result<bool>;

    /// Delete the threads of review `review`, which its writer cancelled
    /// while it was pending.
    fn drop_review_threads(&self, tx: &Transaction<'_>, review: ReviewId) -> Result<()>;

    /// Let the threads of review `review`, which its writer submitted, post
    /// as any thread does; it tells no one, since the review's own alert
    /// names its threads.
    fn release_review_threads(&self, tx: &Transaction<'_>, review: ReviewId) -> Result<()>;

    /// The threads review `review` opened, in the order it opened them.
    fn review_threads(&self, db: &Connection, review: ReviewId) -> Result<Vec<Thread>>;

    /// The threads whose records are `ids`, in that order.
    fn threads(&self, db: &Connection, ids: &[i64]) -> Result<Vec<Thread>>;

    /// The posts of their holders that end the open `review` threads Shoal
    /// opened for a review, which wait for the reviewer's answer.
    fn awaiting_answers(&self, db: &Connection) -> Result<Vec<AwaitedPost>>;
}
