use serde::{Deserialize, Serialize};
use shoal_refs::{BranchName, CommitId};

use super::{Forwarding, Target};
use crate::config::hooks::HookKind;
use crate::forge::numbers::PullNumber;
use crate::space_names::SpaceSelector;
use shoal_ids::checkpoint_numbers::CheckpointNumber;

/// Requests that create, inspect and change workspaces.
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
    /// Create a workspace on a new branch in the repository of the space
    /// `parent`, as its child.
    CreateChild {
        parent: SpaceSelector,
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
    /// Open PR `url`'s `head` in a workspace at a commit: the one made for
    /// or linked to the PR, moved to the new head, else a new one below
    /// `parent`, or else a top-level one.
    OpenPullHead {
        repository: crate::repository::Selector,
        url: crate::forge::items::reference::ItemRef,
        head: PullHead,
        parent: Option<SpaceSelector>,
        #[serde(default, skip_serializing_if = "CreationOptions::is_default")]
        options: CreationOptions,
    },
    AdoptWorkspace {
        repository: crate::repository::Selector,
        path: std::path::PathBuf,
        copy: bool,
    },
    /// Rename the workspace's Git branch, leaving the space's name alone.
    RenameBranch {
        workspace: SpaceSelector,
        branch: String,
    },
    /// Git worktrees under repositories' Shoal directories that no
    /// workspace owns; in pages.
    UntrackedWorktrees,
    /// Record or clear the workspace whose branch this one builds on.
    SetBaseWorkspace {
        workspace: SpaceSelector,
        base: Option<String>,
    },
    InspectWorkspace {
        workspace: SpaceSelector,
    },
    /// The workspace a copy takes its files from, as `source` names it: a
    /// ready one for its current files, or with a checkpoint any one, or a
    /// removed one, with its repository.
    CopyOrigin {
        source: CopySource,
    },
    /// The workspace record once its worktree is verified as its own, and
    /// with `on_branch` that it is on its recorded branch. The CLI asks
    /// before it changes Git state through it.
    VerifyWorkspace {
        workspace: SpaceSelector,
        on_branch: bool,
    },
    /// Fast-forward the workspace recorded on `branch` to the fetched
    /// `origin/<branch>`, refusing a worktree that is not the recorded one on it.
    FastForwardWorkspace {
        workspace: SpaceSelector,
        branch: BranchName,
    },
    /// Bring the workspace up to date with what it was made from, unless it
    /// holds work of its own.
    RefreshWorkspace {
        workspace: SpaceSelector,
    },
    /// Keep where an agent of the workspace keeps its configuration, the
    /// [`AGENT_CONFIG_VARIABLES`](crate::env::AGENT_CONFIG_VARIABLES) it runs
    /// with, before Shoal trusts the workspace there. The repository's
    /// removal or move cleans the entries.
    KeepAgentEnvironment {
        workspace: SpaceSelector,
        environment: std::collections::BTreeMap<String, String>,
    },
    /// Create the worktree the recorded workspace is still to get, as its
    /// preparation does first, answering with the workspace it leaves.
    CreatePendingWorktree {
        workspace: SpaceSelector,
    },
    /// One effective hook, resolved against the directory where it runs.
    WorkspaceHook {
        workspace: SpaceSelector,
        kind: HookKind,
    },
}

/// What a new workspace leaves out, for workspaces on a branch and at a
/// commit alike.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreationOptions {
    /// The new workspace's name, which otherwise derives from what it is
    /// created for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<crate::space_names::SpaceName>,
    /// Skip setup and the hooks around it until `shoal setup` runs them.
    /// The seed clone still happens.
    #[serde(default)]
    pub skip_setup: bool,
    /// Nothing in it is worth keeping: every removal deletes it whatever it
    /// holds, without asking or keeping a checkpoint, and deletes its branch.
    #[serde(default)]
    pub disposable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<shoal_schedule::priority::Priority>,
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

/// The workspace a copy takes its files from, live or removed, and its
/// repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopyOrigin {
    pub workspace: shoal_ids::space_ids::SpaceId,
    pub repository: shoal_ids::repository_ids::RepositoryId,
}

/// PR `number`'s head, which `origin` serves under `reference`, expected at
/// `commit`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullHead {
    pub number: PullNumber,
    pub reference: String,
    pub commit: CommitId,
    /// The fetched ref of the PR's base branch, which the workspace's diff
    /// compares with, so it shows the PR's changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
}

impl WorkspaceMethod {
    /// A request about a workspace's state goes to its machine. A creation,
    /// a listing, and what the caller's execution reads stay with the caller.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::ReopenWorkspace { .. }
            | Self::RenameBranch { .. }
            | Self::SetBaseWorkspace { .. }
            | Self::InspectWorkspace { .. }
            | Self::FastForwardWorkspace { .. }
            | Self::RefreshWorkspace { .. } => Forwarding::Online,
            Self::OpenBranch { .. }
            | Self::CreateWorkspace { .. }
            | Self::CreateChild { .. }
            | Self::CreateCommitWorkspace { .. }
            | Self::OpenPullHead { .. }
            | Self::AdoptWorkspace { .. }
            | Self::UntrackedWorktrees
            | Self::CopyOrigin { .. }
            | Self::VerifyWorkspace { .. }
            | Self::KeepAgentEnvironment { .. }
            | Self::CreatePendingWorktree { .. }
            | Self::WorkspaceHook { .. } => Forwarding::Never,
        }
    }

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::ReopenWorkspace { workspace }
            | Self::RenameBranch { workspace, .. }
            | Self::SetBaseWorkspace { workspace, .. }
            | Self::InspectWorkspace { workspace }
            | Self::CopyOrigin {
                source: CopySource { workspace, .. },
            }
            | Self::VerifyWorkspace { workspace, .. }
            | Self::FastForwardWorkspace { workspace, .. }
            | Self::RefreshWorkspace { workspace }
            | Self::KeepAgentEnvironment { workspace, .. }
            | Self::CreatePendingWorktree { workspace }
            | Self::WorkspaceHook { workspace, .. } => Some(Target::Space(workspace)),
            // A creation names the space it goes below, which a start below
            // another machine's space takes where the caller runs.
            Self::OpenBranch { .. }
            | Self::CreateWorkspace { .. }
            | Self::CreateChild { .. }
            | Self::CreateCommitWorkspace { .. }
            | Self::OpenPullHead { .. }
            | Self::AdoptWorkspace { .. }
            | Self::UntrackedWorktrees => None,
        }
    }
}
