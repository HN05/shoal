//! What leases need of the rest of the daemon, which implements it: the
//! spaces they belong to, their repositories' settings and hooks, the agents
//! that acquire them and the approval requests that gate them.
use std::path::{Path, PathBuf};

use anyhow::Result;
use futures_util::future::BoxFuture;
use rusqlite::{Connection, Transaction};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::approvals::{ApprovalRequest, Target};
use shoal_protocol::space_names::SpaceSelector;

use crate::{
    callers::Caller,
    config::{Config, Effective},
    hooks::{Hook, HookKind},
    model::{Repository, Workspace},
    notifications::Coordinators,
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
    /// The agent execution a request from `caller` comes from: the one its
    /// execution runs, else the one whose process tree registered it.
    fn caller_agent<'a>(&'a self, caller: &'a Caller) -> BoxFuture<'a, Option<ExecutionId>>;

    /// Whether `execution` runs a demo.
    fn runs_demo<'a>(&'a self, execution: &'a ExecutionId) -> BoxFuture<'a, Result<bool>>;
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

/// The approval records of leases, read and written in the transaction that
/// claims or releases one, so a decision never lands between the check and
/// the claim. Its methods run on the store's thread, so they are not
/// futures.
pub trait Grants: Send + Sync {
    /// Record `request` unless a grant covers it, returning the request
    /// still waiting for or refused a decision, or `None` once granted.
    fn check(
        &self,
        tx: &Transaction<'_>,
        request: ApprovalRequest,
    ) -> Result<Option<ApprovalRequest>>;

    /// Cancel the request of `owner` for `name` of `target`, or end its
    /// grant, returning whether there was one.
    fn release(
        &self,
        tx: &Transaction<'_>,
        owner: &SpaceId,
        target: &Target,
        name: &str,
    ) -> Result<bool>;

    /// The active request of `owner` for `name` of `target`.
    fn current(
        &self,
        db: &Connection,
        owner: &SpaceId,
        target: &Target,
        name: &str,
    ) -> Result<Option<ApprovalRequest>>;
}
