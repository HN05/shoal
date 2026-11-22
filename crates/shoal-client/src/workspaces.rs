//! Workspaces: creating, adopting and inspecting them, their branches and
//! bases, and the Git state the daemon reads and changes in their
//! worktrees.
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::status::Inspection;
use std::{collections::BTreeMap, path::PathBuf};

use anyhow::Result;

use crate::connection::{Daemon, Prepared, call, pages, request};
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::{
    config::hooks::HookKind,
    model::{Refreshed, UntrackedWorktree, Workspace},
    protocol::{CommitStart, CreationOptions, WorkspaceMethod},
    repository::Selector,
};

/// `workspace` with its links, completion, executions, places and leases.
pub fn inspect(workspace: SpaceSelector) -> Prepared<Inspection> {
    Prepared::new(WorkspaceMethod::InspectWorkspace { workspace })
}

/// Create a workspace of `repository` at a commit `start` names, in the chat
/// `parent`.
pub async fn create_at_commit(
    daemon: &impl Daemon,
    repository: Selector,
    parent: Option<SpaceSelector>,
    start: CommitStart,
    options: CreationOptions,
) -> Result<Workspace> {
    let method = WorkspaceMethod::CreateCommitWorkspace {
        repository,
        parent,
        start,
        options,
    };
    request(daemon, method).await
}

/// Adopt the worktree at `path` of `repository` as a workspace, copying its
/// files to the workspace directory when `copy`.
pub async fn adopt(
    daemon: &impl Daemon,
    repository: Selector,
    path: PathBuf,
    copy: bool,
) -> Result<Workspace> {
    let method = WorkspaceMethod::AdoptWorkspace {
        repository,
        path,
        copy,
    };
    request(daemon, method).await
}

/// Rename `workspace`'s branch to `branch`.
pub async fn rename_branch(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    branch: String,
) -> Result<Workspace> {
    request(daemon, WorkspaceMethod::RenameBranch { workspace, branch }).await
}

/// Build `workspace` on the workspace `base`, or on the default branch
/// without one.
pub async fn set_base(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    base: Option<String>,
) -> Result<Workspace> {
    request(
        daemon,
        WorkspaceMethod::SetBaseWorkspace { workspace, base },
    )
    .await
}

/// `workspace`, once its worktree is verified as its own, and on its branch
/// when `on_branch`.
pub async fn verify(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    on_branch: bool,
) -> Result<Workspace> {
    request(
        daemon,
        WorkspaceMethod::VerifyWorkspace {
            workspace,
            on_branch,
        },
    )
    .await
}

/// Fast-forward `workspace` to `branch`; whether it moved.
pub async fn fast_forward(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    branch: String,
) -> Result<bool> {
    request(
        daemon,
        WorkspaceMethod::FastForwardWorkspace {
            workspace,
            branch: branch.into(),
        },
    )
    .await
}

/// Bring `workspace` up to date with what it was made from.
pub async fn refresh(daemon: &impl Daemon, workspace: SpaceSelector) -> Result<Refreshed> {
    request(daemon, WorkspaceMethod::RefreshWorkspace { workspace }).await
}

/// The worktrees of registered repositories that no workspace owns.
pub async fn untracked_worktrees(daemon: &impl Daemon) -> Result<Vec<UntrackedWorktree>> {
    pages(daemon, WorkspaceMethod::UntrackedWorktrees).await
}

/// Keep where an agent of `workspace` keeps its configuration, the agent
/// config variables it runs with, inherited ones included, for the
/// repository's removal or move.
pub async fn keep_agent_environment(
    daemon: &impl Daemon,
    workspace: &SpaceId,
    environment: BTreeMap<String, String>,
) -> Result<()> {
    call(
        daemon,
        WorkspaceMethod::KeepAgentEnvironment {
            workspace: SpaceSelector::id(workspace),
            environment,
        },
    )
    .await?;
    Ok(())
}

/// Create the worktree the recorded `workspace` is still to get, returning
/// the workspace it leaves.
pub async fn create_pending_worktree(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
) -> Result<Workspace> {
    request(daemon, WorkspaceMethod::CreatePendingWorktree { workspace }).await
}

/// The command of `workspace`'s hook of `kind`, if its configuration has one.
pub async fn hook(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    kind: HookKind,
) -> Result<Option<PathBuf>> {
    request(daemon, WorkspaceMethod::WorkspaceHook { workspace, kind }).await
}
