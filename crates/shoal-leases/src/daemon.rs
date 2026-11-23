//! What leases need of the rest of the daemon, which implements it: the
//! spaces they belong to, their repositories' settings and hooks, the agents
//! that acquire them and the approval requests that gate them.
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use futures_util::future::BoxFuture;
use rusqlite::{Connection, Transaction};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::approvals::{ApprovalRequest, Target};
use shoal_protocol::space_names::{SpaceName, SpaceSelector};

use crate::{
    callers::Caller,
    config::{Config, Effective},
    hooks::{Hook, HookKind},
    model::{Repository, Workspace},
    notifications::Coordinators,
    time::UnixTime,
};

/// The workspaces leases belong to.
pub trait Spaces: Send + Sync {
    /// The workspace `selector` names, refusing a chat.
    fn workspace<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Workspace>>;

    /// The workspace `id`, as it is now.
    fn workspace_by_id<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Workspace>>;

    /// Record a request of the workspace `id` as its activity, which keeps
    /// it from idle cleanup. A failure is logged.
    fn touch<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, ()>;

    /// Prove that the worktree of `workspace` is still its own, before a
    /// hook runs there.
    fn verify_worktree<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, Result<()>>;

    /// Fail the setup of the workspace `id` with `message` while it is
    /// ready; a lifecycle operation that reserved it meanwhile, such as a
    /// removal, keeps its own state.
    fn fail_setup<'a>(&'a self, id: &'a SpaceId, message: String) -> BoxFuture<'a, Result<()>>;
}

/// The settings of the repositories leases are configured in.
pub trait Settings: Send + Sync {
    /// The settings of `workspace` after every layer, over the global
    /// `config` the caller also reads, so one operation never combines two
    /// versions.
    fn settings<'a>(
        &'a self,
        config: &'a Config,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<Effective>>;

    /// The executable of the hook `kind` of `workspace`, where its settings
    /// configure one.
    fn hook<'a>(
        &'a self,
        workspace: &'a Workspace,
        kind: HookKind,
    ) -> BoxFuture<'a, Result<Option<PathBuf>>>;

    /// Run `hook` of `workspace` with its `executable`, in the environment
    /// its settings give hooks, without waiting for what it leaves running.
    fn run_hook<'a>(
        &'a self,
        workspace: &'a Workspace,
        hook: Hook<'a>,
        executable: &'a Path,
    ) -> BoxFuture<'a, Result<()>>;

    /// The registered repositories, which a pool's members borrow.
    fn repositories(&self) -> BoxFuture<'_, Result<Vec<Repository>>>;
}

/// The agents and demos that acquire leases, which end with them.
pub trait Agents: Send + Sync {
    /// The space of the agent a request from `caller` comes from: the agent
    /// its execution runs, else the one whose process tree registered it.
    fn caller_agent<'a>(&'a self, caller: &'a Caller) -> BoxFuture<'a, Option<SpaceId>>;

    /// Whether `execution` runs a demo.
    fn runs_demo<'a>(&'a self, execution: &'a ExecutionId) -> BoxFuture<'a, Result<bool>>;

    /// Every execution with a record, which a recorded end deletes.
    fn recorded_executions(&self) -> BoxFuture<'_, Result<HashSet<ExecutionId>>>;

    /// The spaces whose agent has not ended for good: it runs, sleeps, waits
    /// in the agent queue for its restore, or restarts.
    fn agent_spaces(&self) -> BoxFuture<'_, Result<HashSet<SpaceId>>>;

    /// Whether an agent launch waits to start in the space `id`.
    fn has_queued_launch<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<bool>>;

    /// The asleep agents, which keep their leases while they sleep, the one
    /// whose space ranks lowest in the agent queue first.
    fn sleepers(&self) -> BoxFuture<'_, Result<Vec<Sleeper>>>;

    /// Tell the agent of the space `id` `message` once it runs again,
    /// without waking it.
    fn tell_agent<'a>(&'a self, id: &'a SpaceId, message: String) -> BoxFuture<'a, ()>;
}

/// An asleep agent, as the leases it keeps read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sleeper {
    /// Its space.
    pub space: SpaceId,
    /// When its harness stopped.
    pub since: UnixTime,
    /// How long it keeps the leases that keep something running: its
    /// space's `[sleep] keep_leases_minutes`.
    pub keeps_running: Duration,
}

impl Sleeper {
    /// Whether it has slept past how long it keeps its running leases.
    pub fn overdue(&self, now: UnixTime) -> bool {
        now.duration_since(self.since) >= self.keeps_running
    }
}

/// The approval requests a lease that needs one waits for.
pub trait Approvals: Send + Sync {
    /// Tell whoever decides the pending `request` about it.
    fn announce<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, ()>;
}

/// Everything leases need of the daemon, which each of their operations
/// that reaches beyond them takes.
pub trait Daemon: Spaces + Settings + Agents + Approvals + Coordinators {}

impl<T: Spaces + Settings + Agents + Approvals + Coordinators> Daemon for T {}

/// The records of other subsystems that leases read or write in the
/// transaction that claims or releases one, so nothing they check changes
/// before the claim commits: a decision never lands between the check and
/// the claim. Its methods run on the store's thread, so they are not
/// futures.
pub trait Records: Send + Sync {
    /// Record the approval `request` unless a grant covers it, returning
    /// the request still waiting for or refused a decision, or `None` once
    /// granted.
    fn check_approval(
        &self,
        tx: &Transaction<'_>,
        request: ApprovalRequest,
    ) -> Result<Option<ApprovalRequest>>;

    /// Cancel the approval request of `owner` for `name` of `target`, or end
    /// its grant, returning whether there was one.
    fn release_approval(
        &self,
        tx: &Transaction<'_>,
        owner: &SpaceId,
        target: &Target,
        name: &str,
    ) -> Result<bool>;

    /// The active approval request of `owner` for `name` of `target`.
    fn current_approval(
        &self,
        db: &Connection,
        owner: &SpaceId,
        target: &Target,
        name: &str,
    ) -> Result<Option<ApprovalRequest>>;

    /// Why a pool member cannot borrow the registered `repository` now, or
    /// `None` when it can.
    fn unborrowable(
        &self,
        db: &Connection,
        repository: &Repository,
    ) -> Result<Option<Unborrowable>>;

    /// The name of the space `id`, while it exists.
    fn space_name(&self, db: &Connection, id: &SpaceId) -> Result<Option<SpaceName>>;

    /// The repository of the space `id`, while it exists and has one.
    fn space_repository(&self, db: &Connection, id: &SpaceId) -> Result<Option<RepositoryId>>;
}

/// Why a registered repository cannot be borrowed, in the order the checks
/// run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unborrowable {
    /// It is no longer registered at the path it was resolved at.
    Registration,
    /// Its removal is incomplete.
    Removal,
    /// Its move is incomplete.
    Move,
    /// A managed workspace's worktree is at its checkout's path.
    Worktree,
}
