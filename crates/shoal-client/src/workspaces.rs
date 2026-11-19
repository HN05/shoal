//! Workspaces: listing, creating and adopting them, their names,
//! bases and places, their status, environment and settings, and the Git
//! state the daemon reads in their worktrees; and the spaces, workspaces and
//! chats together.
use shoal_ids::space_ids::SpaceId;
use std::{collections::BTreeMap, path::PathBuf};

use anyhow::Result;
use serde::de::DeserializeOwned;

use crate::connection::{Daemon, call, pages, request};
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::{
    config::hooks::HookKind,
    model::{
        Blocker, ConflictCheck, DiffBase, Inspection, MergeOptions, MergePreview, OwnedPlace,
        Refreshed, RenamedSpace, Space, SpacePlaces, SpaceStatus, SpaceTarget, UntrackedWorktree,
        Workspace,
    },
    protocol::{CommitStart, ConfigTarget, CreationOptions, RepositoryLayers, WorkspaceMethod},
    repository::Selector,
    sessions::{Origin, PlaceClose, PlaceOwnership},
};

/// `workspace` with its links, completion, executions, places and leases.
pub async fn inspect(daemon: &impl Daemon, workspace: SpaceSelector) -> Result<Inspection> {
    request(daemon, WorkspaceMethod::InspectWorkspace { workspace }).await
}

/// The workspace or chat `selector` names, by number, ID or name.
pub async fn present_space(
    daemon: &impl Daemon,
    selector: &SpaceSelector,
) -> Result<Option<Space>> {
    Ok(crate::spaces::list(daemon).await?.find(selector))
}

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
    pub parent: Option<SpaceSelector>,
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

/// Refuse `blockers` that a launch in `repository` cannot wait for.
pub async fn check_blockers(
    daemon: &impl Daemon,
    repository: Selector,
    blockers: Vec<Blocker>,
) -> Result<()> {
    request(
        daemon,
        WorkspaceMethod::CheckBlockers {
            repository,
            blockers,
        },
    )
    .await
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

/// Give the workspace or chat `space` the name `name`.
pub async fn rename_space(
    daemon: &impl Daemon,
    space: SpaceSelector,
    name: shoal_protocol::space_names::SpaceName,
) -> Result<RenamedSpace> {
    request(daemon, WorkspaceMethod::RenameSpace { space, name }).await
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

/// The spaces `target` selects.
pub async fn find_spaces(daemon: &impl Daemon, target: SpaceTarget) -> Result<Vec<Space>> {
    request(daemon, WorkspaceMethod::FindSpaces { target }).await
}

/// What `space` is doing: its links, agents, leases and assignment.
pub async fn status(daemon: &impl Daemon, space: SpaceSelector) -> Result<SpaceStatus> {
    let status: Box<SpaceStatus> = request(daemon, WorkspaceMethod::SpaceStatus { space }).await?;
    Ok(*status)
}

/// The worktrees of registered repositories that no workspace owns.
pub async fn untracked_worktrees(daemon: &impl Daemon) -> Result<Vec<UntrackedWorktree>> {
    pages(daemon, WorkspaceMethod::UntrackedWorktrees).await
}

/// The places Shoal created for every space, a workspace or a chat.
pub async fn owned_places(daemon: &impl Daemon) -> Result<Vec<OwnedPlace>> {
    request(daemon, WorkspaceMethod::OwnedPlaces).await
}

/// Where the executions of `space`, a workspace or a chat, run, and the
/// places Shoal created for it.
pub async fn places(daemon: &impl Daemon, space: SpaceSelector) -> Result<SpacePlaces> {
    request(daemon, WorkspaceMethod::SpacePlaces { space }).await
}

/// Record `origin`, a place Shoal created for `workspace`, so the daemon
/// closes it once the workspace is removed.
pub async fn record_owned_place(
    daemon: &impl Daemon,
    workspace: &SpaceId,
    origin: Origin,
) -> Result<()> {
    call(
        daemon,
        WorkspaceMethod::RecordOwnedPlace {
            workspace: SpaceSelector::id(workspace),
            origin,
        },
    )
    .await?;
    Ok(())
}

/// Whose `place`, the place of a terminal host, is for `space`, as the
/// daemon knows it.
pub async fn place_ownership(
    daemon: &impl Daemon,
    space: &SpaceId,
    place: &Origin,
) -> Result<PlaceOwnership> {
    request(
        daemon,
        WorkspaceMethod::PlaceOwnership {
            space: SpaceSelector::id(space),
            place: place.clone(),
        },
    )
    .await
}

/// Close `place`, a place Shoal created for `workspace`, while the daemon
/// still records it as that workspace's.
pub async fn close_owned_place(
    daemon: &impl Daemon,
    workspace: &SpaceId,
    place: &Origin,
) -> Result<PlaceClose> {
    request(
        daemon,
        WorkspaceMethod::CloseOwnedPlace {
            workspace: workspace.clone(),
            place: place.clone(),
        },
    )
    .await
}

/// The effective permissions of an agent starting in `mode` in `workspace`.
pub async fn launch_permissions(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    mode: shoal_protocol::config::modes::ModeName,
    narrowing: shoal_protocol::config::permissions::PermissionsConfig,
) -> Result<shoal_protocol::config::permissions::Permissions> {
    let method = WorkspaceMethod::LaunchPermissions {
        workspace,
        mode,
        narrowing,
    };
    request(daemon, method).await
}

/// The effective permissions of this scoped process's own space for an
/// action in the registered repository `repository`.
pub async fn held_permissions(
    daemon: &impl Daemon,
    repository: Option<shoal_protocol::repository::Selector>,
) -> Result<shoal_protocol::config::permissions::Permissions> {
    request(daemon, WorkspaceMethod::HeldPermissions { repository }).await
}

/// Keep where an agent of `workspace` keeps its configuration for the
/// repository's removal.
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

/// The repository configuration layers that apply to `target`, below the
/// global configuration, read as `L`.
pub async fn config_layers<L: DeserializeOwned>(
    daemon: &impl Daemon,
    target: ConfigTarget,
) -> Result<L> {
    let layers: RepositoryLayers =
        request(daemon, WorkspaceMethod::LayeredConfig { target }).await?;
    layers.read()
}

/// The commit `workspace`'s changes are counted from.
pub async fn diff_base(daemon: &impl Daemon, workspace: SpaceSelector) -> Result<DiffBase> {
    request(daemon, WorkspaceMethod::DiffBase { workspace }).await
}

/// The files where `workspace`'s committed HEAD conflicts with `target`, by
/// default its base.
pub async fn conflicts(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    target: Option<String>,
) -> Result<ConflictCheck> {
    request(daemon, WorkspaceMethod::Conflicts { workspace, target }).await
}

/// What merging `workspace`'s branch as `options` say would do.
pub async fn preview_merge(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    options: &MergeOptions,
) -> Result<MergePreview> {
    let preview = WorkspaceMethod::PreviewMerge {
        workspace,
        push: options.push,
        method: options.method,
        message: options.message.clone(),
    };
    request(daemon, preview).await
}
