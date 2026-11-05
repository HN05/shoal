//! Workspaces: creating and adopting them, their names, titles, bases and
//! places, their status and environment, and the Git state the daemon reads
//! in their worktrees.
use std::{collections::BTreeMap, path::PathBuf};

use anyhow::Result;

use crate::{
    client::{
        connection::{Daemon, pages, request},
        repository::Selector,
    },
    config::{hooks::HookKind, repo::ConfigLayers},
    model::{
        ConflictCheck, Dependency, DiffBase, MergePreview, OwnedPlace, PresentSpace, SpaceStatus,
        SpaceTarget, UntrackedWorktree, Workspace,
    },
    protocol::{CommitStart, ConfigTarget, CreationOptions, WorkspaceMethod},
};

/// A workspace whose creation [`check_creation`] checks: on the new branch
/// `branch` or the existing one `existing`, or named by default.
pub struct PlannedWorkspace {
    pub repository: Selector,
    pub branch: Option<String>,
    pub existing: Option<String>,
    pub base: Option<String>,
    pub git_profile: Option<String>,
    pub path: Option<PathBuf>,
    /// The chat the workspace is created in.
    pub parent: Option<String>,
    pub options: CreationOptions,
}

/// Refuse what creating `planned` would refuse, before anything is asked or
/// queued.
pub async fn check_creation(daemon: &impl Daemon, planned: PlannedWorkspace) -> Result<()> {
    let PlannedWorkspace {
        repository,
        branch,
        existing,
        base,
        git_profile,
        path,
        parent,
        options,
    } = planned;
    let method = WorkspaceMethod::CheckCreation {
        repository,
        branch,
        existing,
        base,
        git_profile,
        path,
        options,
        parent,
    };
    request(daemon, method).await
}

/// Refuse dependencies `after` that `repository` cannot wait for.
pub async fn check_dependencies(
    daemon: &impl Daemon,
    repository: Selector,
    after: Vec<Dependency>,
) -> Result<()> {
    request(
        daemon,
        WorkspaceMethod::CheckDependencies { repository, after },
    )
    .await
}

/// Create a workspace of `repository` at a commit `start` names, in the chat
/// `parent`.
pub async fn create_at_commit(
    daemon: &impl Daemon,
    repository: Selector,
    parent: Option<String>,
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
pub async fn rename(daemon: &impl Daemon, workspace: String, branch: String) -> Result<Workspace> {
    request(
        daemon,
        WorkspaceMethod::RenameWorkspace { workspace, branch },
    )
    .await
}

/// Give `workspace` the title its places show.
pub async fn set_title(
    daemon: &impl Daemon,
    workspace: String,
    title: String,
) -> Result<Workspace> {
    let method = WorkspaceMethod::SetWorkspaceTitle {
        workspace,
        title: Some(title),
    };
    request(daemon, method).await
}

/// Build `workspace` on the workspace `base`, or on the default branch
/// without one.
pub async fn set_base(
    daemon: &impl Daemon,
    workspace: String,
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
pub async fn verify(daemon: &impl Daemon, workspace: String, on_branch: bool) -> Result<Workspace> {
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
pub async fn fast_forward(daemon: &impl Daemon, workspace: String, branch: String) -> Result<bool> {
    request(
        daemon,
        WorkspaceMethod::FastForwardWorkspace { workspace, branch },
    )
    .await
}

/// The spaces `target` selects.
pub async fn find_spaces(daemon: &impl Daemon, target: SpaceTarget) -> Result<Vec<PresentSpace>> {
    request(daemon, WorkspaceMethod::FindSpaces { target }).await
}

/// What `space` is doing: its links, agents, leases and assignment.
pub async fn status(daemon: &impl Daemon, space: String) -> Result<SpaceStatus> {
    let status: Box<SpaceStatus> = request(daemon, WorkspaceMethod::SpaceStatus { space }).await?;
    Ok(*status)
}

/// The worktrees of registered repositories that no workspace owns.
pub async fn untracked_worktrees(daemon: &impl Daemon) -> Result<Vec<UntrackedWorktree>> {
    pages(daemon, WorkspaceMethod::UntrackedWorktrees).await
}

/// The places Shoal created for every workspace.
pub async fn owned_places(daemon: &impl Daemon) -> Result<Vec<OwnedPlace>> {
    request(daemon, WorkspaceMethod::OwnedPlaces).await
}

/// The places Shoal created for `workspace`.
pub async fn places(daemon: &impl Daemon, workspace: String) -> Result<Vec<OwnedPlace>> {
    request(daemon, WorkspaceMethod::WorkspacePlaces { workspace }).await
}

/// The environment `workspace`'s configuration gives its commands.
pub async fn environment(
    daemon: &impl Daemon,
    workspace: String,
) -> Result<BTreeMap<String, String>> {
    request(daemon, WorkspaceMethod::WorkspaceEnv { workspace }).await
}

/// Revoke the environment `token` handed out for `workspace`.
pub async fn revoke_environment(
    daemon: &impl Daemon,
    workspace: String,
    token: String,
) -> Result<()> {
    request(
        daemon,
        WorkspaceMethod::RevokeWorkspaceEnv { workspace, token },
    )
    .await
}

/// The command of `workspace`'s hook of `kind`, if its configuration has one.
pub async fn hook(
    daemon: &impl Daemon,
    workspace: String,
    kind: HookKind,
) -> Result<Option<PathBuf>> {
    request(daemon, WorkspaceMethod::WorkspaceHook { workspace, kind }).await
}

/// The repository configuration layers that apply to `target`, below the
/// global configuration.
pub async fn config_layers(daemon: &impl Daemon, target: ConfigTarget) -> Result<ConfigLayers> {
    let layers: Box<ConfigLayers> =
        request(daemon, WorkspaceMethod::LayeredConfig { target }).await?;
    Ok(*layers)
}

/// The commit `workspace`'s changes are counted from.
pub async fn diff_base(daemon: &impl Daemon, workspace: String) -> Result<DiffBase> {
    request(daemon, WorkspaceMethod::DiffBase { workspace }).await
}

/// The files where `workspace`'s committed HEAD conflicts with `target`, by
/// default its base.
pub async fn conflicts(
    daemon: &impl Daemon,
    workspace: String,
    target: Option<String>,
) -> Result<ConflictCheck> {
    request(daemon, WorkspaceMethod::Conflicts { workspace, target }).await
}

/// What merging `workspace`'s branch, and pushing the result when `push`,
/// would do.
pub async fn preview_merge(
    daemon: &impl Daemon,
    workspace: String,
    push: bool,
) -> Result<MergePreview> {
    request(daemon, WorkspaceMethod::PreviewMerge { workspace, push }).await
}
