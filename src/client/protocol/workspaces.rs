use serde::{Deserialize, Serialize};

use super::ConfigTarget;
use super::Lifetime;
use crate::config::hooks::HookKind;

/// Requests that create, find, inspect and change workspaces.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceMethod {
    OpenBranch {
        path: Option<std::path::PathBuf>,
        repository: crate::client::repository::Selector,
        branch: String,
        git_profile: Option<String>,
        base: Option<String>,
        /// The chat opening the workspace; `None` gives it to the user.
        chat: Option<String>,
        /// Apply only when opening creates a new worktree.
        #[serde(default)]
        options: CreationOptions,
    },
    CreateWorkspace {
        path: Option<std::path::PathBuf>,
        repository: crate::client::repository::Selector,
        name: String,
        base: Option<String>,
        git_profile: Option<String>,
        /// The chat opening the workspace; `None` gives it to the user.
        chat: Option<String>,
        #[serde(default)]
        options: CreationOptions,
    },
    /// Reopen the existing `workspace` for a launch in it, never creating one.
    ReopenWorkspace {
        workspace: String,
    },
    /// Create a workspace on a new branch in `workspace`'s repository, split
    /// from it as its child.
    SplitWorkspace {
        workspace: String,
        name: String,
        base: Option<String>,
        #[serde(default)]
        options: CreationOptions,
    },
    /// Create a workspace at a commit, without a branch: a child of
    /// `parent`, or without one a top-level workspace given to `chat`, or
    /// else to the user.
    CreateCommitWorkspace {
        repository: crate::client::repository::Selector,
        parent: Option<String>,
        start: CommitStart,
        chat: Option<String>,
        #[serde(default)]
        options: CreationOptions,
    },
    /// Refuse what `CreateWorkspace` or `OpenBranch` would refuse in this
    /// request, without creating anything.
    CheckCreation {
        repository: crate::client::repository::Selector,
        /// The new branch; `existing` names a branch to open instead.
        branch: Option<String>,
        existing: Option<String>,
        base: Option<String>,
        git_profile: Option<String>,
        path: Option<std::path::PathBuf>,
    },
    /// Refuse the work `after` names that a queued launch in `repository`
    /// could not wait for, without queuing anything.
    CheckDependencies {
        repository: crate::client::repository::Selector,
        after: Vec<crate::model::Dependency>,
    },
    AdoptWorkspace {
        repository: crate::client::repository::Selector,
        path: std::path::PathBuf,
        copy: bool,
    },
    RenameWorkspace {
        workspace: String,
        branch: String,
    },
    ListWorkspaces,
    /// Git worktrees under repositories' Shoal directories that no
    /// workspace owns; in pages.
    UntrackedWorktrees,
    /// Workspaces that link an item or hold a resource.
    FindWorkspaces {
        target: crate::model::WorkspaceTarget,
    },
    /// Give the workspace a title its places show, or clear it without one.
    SetWorkspaceTitle {
        workspace: String,
        title: Option<String>,
    },
    /// The places Shoal created for the workspace.
    WorkspacePlaces {
        workspace: String,
    },
    /// Record or clear the workspace whose branch this one builds on.
    SetBaseWorkspace {
        workspace: String,
        base: Option<String>,
    },
    InspectWorkspace {
        workspace: String,
    },
    /// The workspace record once its worktree identity is verified, and with
    /// `on_branch` that it is on its recorded branch, before the CLI changes Git
    /// state through it.
    VerifyWorkspace {
        workspace: String,
        on_branch: bool,
    },
    /// Fast-forward the workspace recorded on `branch` to the fetched
    /// `origin/<branch>`, refusing a worktree that is not the recorded one on it.
    FastForwardWorkspace {
        workspace: String,
        branch: String,
    },
    WorkspaceEnv {
        workspace: String,
    },
    RevokeWorkspaceEnv {
        workspace: String,
        token: String,
    },
    WorkspaceStatus {
        workspace: String,
    },
    DiffBase {
        workspace: String,
    },
    Conflicts {
        workspace: String,
        target: Option<String>,
    },
    /// What landing the workspace's HEAD warns about.
    LandWarning {
        workspace: String,
    },
    /// Keep where an agent of the workspace keeps its configuration, before
    /// Shoal trusts the workspace there, until the repository's removal.
    KeepAgentEnvironment {
        workspace: String,
        environment: std::collections::BTreeMap<String, String>,
    },
    /// Record a place Shoal created for the workspace, which it closes once
    /// the workspace is removed.
    RecordOwnedPlace {
        workspace: String,
        origin: crate::client::sessions::Origin,
    },
    /// The places Shoal created for every workspace.
    OwnedPlaces,
    /// Close `place`, a place Shoal created for `workspace`, while the
    /// daemon's record still names that workspace its owner. The workspace
    /// may already be removed.
    CloseOwnedPlace {
        workspace: String,
        place: crate::client::sessions::Origin,
    },
    /// Whose `place` is, as the daemon knows it for `workspace`.
    PlaceOwnership {
        workspace: String,
        place: crate::client::sessions::Origin,
    },
    /// One effective hook, resolved against the directory where it runs.
    WorkspaceHook {
        workspace: String,
        kind: HookKind,
    },
    /// The repository layer of the target's config, for the CLI to resolve
    /// against the global config it reads at launch.
    LayeredConfig {
        target: ConfigTarget,
    },
    WatchWorkspaceEvents {
        since: Option<i64>,
        follow: bool,
    },
}

/// What a new workspace leaves out, for workspaces on a branch and at a
/// commit alike.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreationOptions {
    /// Skip what gates readiness, `setup.command`, `hooks.pre_setup` and
    /// `setup.copy_ignored`, and the hooks around it, `hooks.post_setup` and
    /// the removal hooks, until `shoal work setup` runs it; the seed clone
    /// still happens.
    #[serde(default)]
    pub skip_setup: bool,
}

/// Where a workspace at a commit starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitStart {
    /// A revision as `--base` names it; `None` for the default branch.
    Revision(Option<String>),
    /// PR `number`'s head, which `origin` serves under `reference`, expected
    /// at `commit`.
    PullHead {
        number: u64,
        reference: String,
        commit: String,
    },
}

impl WorkspaceMethod {
    pub fn lifetime(&self) -> Lifetime {
        match self {
            Self::WatchWorkspaceEvents { .. } => Lifetime::Open,
            _ => Lifetime::Reply,
        }
    }
}
