use serde::{Deserialize, Serialize};

use super::ConfigTarget;
use super::Lifetime;
use crate::hooks::HookKind;

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
        /// The chat opening the workspace; `None` gives it to the user.
        chat: Option<String>,
    },
    CreateWorkspace {
        path: Option<std::path::PathBuf>,
        repository: crate::repository::Selector,
        name: String,
        base: Option<String>,
        git_profile: Option<String>,
        /// The chat opening the workspace; `None` gives it to the user.
        chat: Option<String>,
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
        origin: crate::sessions::Origin,
    },
    /// The places Shoal created for every workspace.
    OwnedPlaces,
    /// Close `place`, a place Shoal created for `workspace`, while the
    /// daemon's record still names that workspace its owner. The workspace
    /// may already be removed.
    CloseOwnedPlace {
        workspace: String,
        place: crate::sessions::Origin,
    },
    /// Whose `place` is, as the daemon knows it for `workspace`.
    PlaceOwnership {
        workspace: String,
        place: crate::sessions::Origin,
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

impl WorkspaceMethod {
    pub fn lifetime(&self) -> Lifetime {
        match self {
            Self::WatchWorkspaceEvents { .. } => Lifetime::Open,
            _ => Lifetime::Reply,
        }
    }
}
