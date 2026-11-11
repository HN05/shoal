use serde::{Deserialize, Serialize};

use super::ConfigTarget;
use super::Lifetime;
use crate::config::hooks::HookKind;
use crate::forge::numbers::PullNumber;
use crate::model::CheckpointNumber;
use crate::space_names::SpaceSelector;

/// Requests that create, find, inspect and change workspaces.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceMethod {
    OpenBranch {
        path: Option<std::path::PathBuf>,
        repository: crate::repository::Selector,
        branch: String,
        git_profile: Option<String>,
        base: Option<String>,
        /// The space opening the workspace, which it goes below; `None`
        /// gives it to the user.
        parent: Option<SpaceSelector>,
        /// Apply only when opening creates a new worktree.
        #[serde(default, skip_serializing_if = "CreationOptions::is_default")]
        options: CreationOptions,
    },
    CreateWorkspace {
        path: Option<std::path::PathBuf>,
        repository: crate::repository::Selector,
        name: String,
        base: Option<String>,
        git_profile: Option<String>,
        /// The space opening the workspace, which it goes below; `None`
        /// gives it to the user.
        parent: Option<SpaceSelector>,
        #[serde(default, skip_serializing_if = "CreationOptions::is_default")]
        options: CreationOptions,
    },
    /// Reopen the existing `workspace` for a launch in it, never creating one.
    ReopenWorkspace {
        workspace: SpaceSelector,
    },
    /// Create a workspace on a new branch in `workspace`'s repository, split
    /// from it as its child.
    SplitWorkspace {
        workspace: SpaceSelector,
        name: String,
        base: Option<String>,
        #[serde(default, skip_serializing_if = "CreationOptions::is_default")]
        options: CreationOptions,
    },
    /// Create a workspace at a commit, without a branch: a child of the
    /// space `parent`, or without one a top-level workspace of the user's.
    CreateCommitWorkspace {
        repository: crate::repository::Selector,
        parent: Option<SpaceSelector>,
        start: CommitStart,
        #[serde(default, skip_serializing_if = "CreationOptions::is_default")]
        options: CreationOptions,
    },
    /// Open PR `url`'s `head` in a workspace at a commit: the one in
    /// `repository` made for or linked to the PR, moved to the head when the
    /// PR's head moved, else a new one below the space `parent`, or else a
    /// top-level one of the user's.
    OpenPullHead {
        repository: crate::repository::Selector,
        url: String,
        head: PullHead,
        parent: Option<SpaceSelector>,
        #[serde(default, skip_serializing_if = "CreationOptions::is_default")]
        options: CreationOptions,
    },
    /// Refuse what `CreateWorkspace` or `OpenBranch` would refuse in this
    /// request, without creating anything.
    CheckCreation {
        repository: crate::repository::Selector,
        /// The new branch; `existing` names a branch to open instead.
        branch: Option<String>,
        existing: Option<String>,
        base: Option<String>,
        git_profile: Option<String>,
        path: Option<std::path::PathBuf>,
        #[serde(default, skip_serializing_if = "CreationOptions::is_default")]
        options: CreationOptions,
        /// The space the workspace would be below, which must be in the
        /// repository.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<SpaceSelector>,
    },
    /// Refuse the work `after` names that a queued launch in `repository`
    /// could not wait for, without queuing anything.
    CheckDependencies {
        repository: crate::repository::Selector,
        after: Vec<crate::model::Dependency>,
    },
    AdoptWorkspace {
        repository: crate::repository::Selector,
        path: std::path::PathBuf,
        copy: bool,
    },
    RenameWorkspace {
        workspace: SpaceSelector,
        branch: String,
    },
    ListWorkspaces,
    /// Git worktrees under repositories' Shoal directories that no
    /// workspace owns; in pages.
    UntrackedWorktrees,
    /// The spaces that link an item or the workspaces that hold a resource.
    FindSpaces {
        target: crate::model::SpaceTarget,
    },
    /// Give the workspace a title its places show, or clear it without one.
    SetWorkspaceTitle {
        workspace: SpaceSelector,
        title: Option<String>,
    },
    /// The places Shoal created for the workspace.
    WorkspacePlaces {
        workspace: SpaceSelector,
    },
    /// Record or clear the workspace whose branch this one builds on.
    SetBaseWorkspace {
        workspace: SpaceSelector,
        base: Option<String>,
    },
    InspectWorkspace {
        workspace: SpaceSelector,
    },
    /// The workspace record once its worktree identity is verified, and with
    /// `on_branch` that it is on its recorded branch, before the CLI changes Git
    /// state through it.
    VerifyWorkspace {
        workspace: SpaceSelector,
        on_branch: bool,
    },
    /// Fast-forward the workspace recorded on `branch` to the fetched
    /// `origin/<branch>`, refusing a worktree that is not the recorded one on it.
    FastForwardWorkspace {
        workspace: SpaceSelector,
        branch: String,
    },
    /// The effective permissions of an agent starting in `mode` with
    /// `narrowing` in the workspace, which its launch's push rules follow.
    LaunchPermissions {
        workspace: SpaceSelector,
        mode: crate::config::modes::ModeName,
        #[serde(
            default,
            skip_serializing_if = "crate::config::permissions::PermissionsConfig::is_empty"
        )]
        narrowing: crate::config::permissions::PermissionsConfig,
    },
    /// The effective permissions of the calling scoped process's own space
    /// for an action in the registered repository `repository`, the most
    /// its `--permission` gives; the user holds every permission.
    HeldPermissions {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repository: Option<crate::repository::Selector>,
    },
    WorkspaceEnv {
        workspace: SpaceSelector,
    },
    RevokeWorkspaceEnv {
        workspace: SpaceSelector,
        token: crate::scope_tokens::ScopeToken,
    },
    /// The status of a workspace or chat, running or stopped.
    SpaceStatus {
        space: SpaceSelector,
    },
    /// The sessions the agents of a workspace or chat reported.
    SpaceSessions {
        space: SpaceSelector,
    },
    /// Write a copy of a workspace or chat for another Shoal to the new file
    /// `file`, leaving it running: a workspace at its checkpoint
    /// `checkpoint`, or at a new one.
    CopySpace {
        space: SpaceSelector,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checkpoint: Option<CheckpointNumber>,
        file: std::path::PathBuf,
    },
    DiffBase {
        workspace: SpaceSelector,
    },
    Conflicts {
        workspace: SpaceSelector,
        target: Option<String>,
    },
    /// What merging the workspace into the default branch does, after the
    /// checks the merge makes before it changes anything; with `push`, the
    /// merge pushes the default branch afterwards.
    PreviewMerge {
        workspace: SpaceSelector,
        push: bool,
    },
    /// Keep where an agent of the workspace keeps its configuration, before
    /// Shoal trusts the workspace there, until the repository's removal.
    KeepAgentEnvironment {
        workspace: SpaceSelector,
        environment: std::collections::BTreeMap<String, String>,
    },
    /// Record a place Shoal created for the workspace, which it closes once
    /// the workspace is removed.
    RecordOwnedPlace {
        workspace: SpaceSelector,
        origin: crate::sessions::Origin,
    },
    /// The places Shoal created for every workspace.
    OwnedPlaces,
    /// Close `place`, a place Shoal created for `workspace`, while the
    /// daemon's record still names that workspace its owner. The workspace
    /// may already be removed, so the request names it by ID.
    CloseOwnedPlace {
        workspace: crate::space_ids::SpaceId,
        place: crate::sessions::Origin,
    },
    /// Whose `place` is, as the daemon knows it for `workspace`.
    PlaceOwnership {
        workspace: SpaceSelector,
        place: crate::sessions::Origin,
    },
    /// One effective hook, resolved against the directory where it runs.
    WorkspaceHook {
        workspace: SpaceSelector,
        kind: HookKind,
    },
    /// The repository layer of the target's config, for the CLI to resolve
    /// against the global config it reads at launch.
    LayeredConfig {
        target: ConfigTarget,
    },
    WatchWorkspaceEvents {
        #[serde(rename = "since")]
        after: Option<crate::events::EventCursor>,
        follow: bool,
    },
    /// The newest `limit` events of a space's log, or Shoal's without
    /// `space`, that `filter` shows, oldest first, in pages.
    SpaceLog {
        space: Option<SpaceSelector>,
        filter: crate::log::LogFilter,
        limit: u32,
    },
}

/// What a new workspace leaves out, for workspaces on a branch and at a
/// commit alike.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreationOptions {
    /// Skip what gates readiness, `setup.command`, `hooks.pre_setup` and
    /// `setup.copy_ignored`, and the hooks around it, `hooks.post_setup` and
    /// the removal hooks, until `shoal setup` runs it; the seed clone
    /// still happens.
    #[serde(default)]
    pub skip_setup: bool,
    /// Nothing in it is worth keeping: every removal deletes it whatever it
    /// holds, without asking or keeping a checkpoint, and deletes its branch.
    #[serde(default)]
    pub disposable: bool,
}

impl CreationOptions {
    /// Whether it leaves nothing out, which requests then do not spell.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Where a workspace at a commit starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitStart {
    /// A revision as `--base` names it; `None` for the default branch.
    Revision(Option<String>),
    PullHead(PullHead),
    /// A copy of another workspace, at its HEAD.
    Copy(CopySource),
}

/// The workspace a new one copies: its current files, or one of its
/// checkpoints, of a live or a removed workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopySource {
    /// The workspace's number, name or ID; a removed workspace's name or
    /// ID only with `checkpoint`.
    pub workspace: SpaceSelector,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<CheckpointNumber>,
}

/// PR `number`'s head, which `origin` serves under `reference`, expected at
/// `commit`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullHead {
    pub number: PullNumber,
    pub reference: String,
    pub commit: String,
    /// The fetched ref of the PR's base branch, which the workspace's diff
    /// compares with, so it shows the PR's changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
}

impl WorkspaceMethod {
    pub fn lifetime(&self) -> Lifetime {
        match self {
            Self::WatchWorkspaceEvents { .. } => Lifetime::Open,
            _ => Lifetime::Reply,
        }
    }
}
