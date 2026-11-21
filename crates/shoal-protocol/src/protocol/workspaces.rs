use serde::{Deserialize, Serialize};
use shoal_refs::{BranchName, CommitId};

use super::Lifetime;
use super::{ConfigTarget, Forwarding, Target};
use crate::config::hooks::HookKind;
use crate::forge::numbers::PullNumber;
use crate::space_names::SpaceSelector;
use shoal_ids::checkpoint_numbers::CheckpointNumber;

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
    /// Create a workspace on a new branch in the repository of the space
    /// `parent`, a workspace or a chat, as its child.
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
    /// Open PR `url`'s `head` in a workspace at a commit: the one in
    /// `repository` made for or linked to the PR, moved to the head when the
    /// PR's head moved, else a new one below the space `parent`, or else a
    /// top-level one of the user's.
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
    /// Make the workspace a chat in `mode`, `chat` by default, once its
    /// agent's turn ends: its worktree goes as `shoal rm` removes it, and
    /// its agent restarts in the repository's checkout.
    ConvertToChat {
        workspace: SpaceSelector,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<crate::config::modes::ModeName>,
    },
    /// Git worktrees under repositories' Shoal directories that no
    /// workspace owns; in pages.
    UntrackedWorktrees,
    /// Every space the caller sees, workspaces and chats, running or
    /// stopped.
    ListSpaces,
    /// The spaces that link an item or the workspaces that hold a resource.
    FindSpaces {
        target: crate::model::SpaceTarget,
    },
    /// Where the space `space` names runs: here, or on the other joined
    /// machine the hub lists it on, among the spaces the caller sees.
    LocateSpace {
        space: SpaceSelector,
    },
    /// The spaces `spaces` name, or with `machines` every space of those
    /// joined machines, by name or ID, with those below each on every
    /// machine when `below`, each once and after the space above it, and
    /// where each runs, among the spaces the caller sees.
    SelectSpaces {
        spaces: Vec<SpaceSelector>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        machines: Vec<String>,
        below: bool,
    },
    /// Give the workspace or chat `space` the name `name`; the reply also
    /// names it as it was before.
    RenameSpace {
        space: SpaceSelector,
        name: crate::space_names::SpaceName,
    },
    /// Where the space's executions run and the places Shoal created for
    /// it, a workspace's or a chat's.
    SpacePlaces {
        space: SpaceSelector,
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
        branch: BranchName,
    },
    /// Bring the workspace up to date with what it was made from, unless it
    /// holds work of its own.
    RefreshWorkspace {
        workspace: SpaceSelector,
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
    /// Issue a new scope token that acts for the space `space` until it is
    /// revoked or the space is removed.
    IssueScope {
        space: SpaceSelector,
    },
    /// Revoke `token`, which [`Self::IssueScope`] issued for `space`.
    RevokeScope {
        space: SpaceSelector,
        token: crate::scope_tokens::ScopeToken,
    },
    /// The variables a process of the space `space` runs with, without a
    /// scope token.
    SpaceEnvironment {
        space: SpaceSelector,
    },
    /// The status of a workspace or chat, running or stopped.
    SpaceStatus {
        space: SpaceSelector,
    },
    /// The config file the repository of the space `space` tracks, as
    /// TOML, which a machine without that repository reads a child's modes
    /// under. Answers with [`super::Body::TrackedConfig`].
    TrackedConfig {
        space: SpaceSelector,
    },
    /// The sessions the agents of a workspace or chat reported.
    SpaceSessions {
        space: SpaceSelector,
    },
    /// Follow the session `session` of an agent of a workspace or chat, or
    /// else its agent's current one: the session, then the bytes of its
    /// files as its tool writes them, each from where `from` says the
    /// client read it to, as when it follows again after a daemon restart,
    /// until the client hangs up.
    FollowSession {
        space: SpaceSelector,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<shoal_ids::agent_session_ids::AgentSessionId>,
        #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
        from: std::collections::BTreeMap<std::path::PathBuf, crate::transcripts::ReadTo>,
    },
    /// Attach the terminal `viewer` describes, on another machine or a
    /// device, to an agent's session of a workspace or chat as
    /// `shoal attach` does on the space's machine, which runs it on a
    /// terminal of the viewer's size: answered with [`super::Body::Ok`],
    /// after which the connection carries [`crate::attaching`] frames both
    /// ways until the attach ends. Refused for a space's process.
    AttachSession {
        space: SpaceSelector,
        #[serde(default)]
        viewer: crate::attaching::Viewer,
    },
    /// Write a copy of a workspace or chat, with copies of the spaces below
    /// it that `carrying` carries, for another Shoal to the new file
    /// `file`, leaving them running: a workspace at its checkpoint
    /// `checkpoint`, or at a new one, and those below at new ones.
    CopySpace {
        space: SpaceSelector,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checkpoint: Option<CheckpointNumber>,
        file: std::path::PathBuf,
        #[serde(default)]
        carrying: crate::transfer::Carrying,
    },
    DiffBase {
        workspace: SpaceSelector,
    },
    Conflicts {
        workspace: SpaceSelector,
        target: Option<String>,
    },
    /// The workspace's changes since its branch's fork point, as `git diff`
    /// writes them in its worktree, shown as `display` says: answered with
    /// [`super::Body::Diff`] frames as Git writes, the last with its exit
    /// code. It counts as no use of the workspace.
    Diff {
        workspace: SpaceSelector,
        #[serde(default)]
        display: crate::diffs::DiffDisplay,
    },
    /// What merging the workspace into the default branch by `method`,
    /// with a squash's `message`, does, after the checks the merge makes
    /// before it changes anything; with `push`, the merge pushes the default
    /// branch afterwards.
    PreviewMerge {
        workspace: SpaceSelector,
        push: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        method: Option<crate::forge::items::action::MergeMethod>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
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
        workspace: shoal_ids::space_ids::SpaceId,
        place: crate::sessions::Origin,
    },
    /// Whose `place` is, as the daemon knows it for the workspace or chat
    /// `space`.
    PlaceOwnership {
        space: SpaceSelector,
        place: crate::sessions::Origin,
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
    /// The repository layer of the target's config, for the CLI to resolve
    /// against the global config it reads at launch.
    LayeredConfig {
        target: ConfigTarget,
    },
    WatchSpaceEvents {
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreationOptions {
    /// The new workspace's name, which otherwise derives from what it is
    /// created for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<crate::space_names::SpaceName>,
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
    /// The priority set on it, which ranks its queued work.
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
    pub fn lifetime(&self) -> Lifetime {
        match self {
            Self::WatchSpaceEvents { .. }
            | Self::FollowSession { .. }
            | Self::AttachSession { .. }
            | Self::Diff { .. } => Lifetime::Open,
            _ => Lifetime::Reply,
        }
    }
}

impl WorkspaceMethod {
    /// A request about a space's state goes to its machine, its status
    /// what that machine last reported while it is offline, a follow's or a
    /// diff's answer coming as that machine writes it and an attachment's
    /// connection carrying bytes both ways; a creation, a listing, a copy
    /// written where the caller runs, and what the caller's execution,
    /// terminal or setup reads stay with the caller: the places and
    /// sessions it looks for in its own terminal hosts and files.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::ReopenWorkspace { .. }
            | Self::RenameBranch { .. }
            | Self::ConvertToChat { .. }
            | Self::RenameSpace { .. }
            | Self::SetBaseWorkspace { .. }
            | Self::InspectWorkspace { .. }
            | Self::FastForwardWorkspace { .. }
            | Self::RefreshWorkspace { .. }
            | Self::LaunchPermissions { .. }
            | Self::TrackedConfig { .. }
            | Self::DiffBase { .. }
            | Self::Conflicts { .. }
            | Self::PreviewMerge { .. }
            | Self::LayeredConfig { .. }
            | Self::SpaceLog { .. } => Forwarding::Online,
            Self::SpaceStatus { .. } => Forwarding::Reported,
            Self::FollowSession { .. } | Self::Diff { .. } => Forwarding::Streamed,
            Self::AttachSession { .. } => Forwarding::Duplex,
            Self::OpenBranch { .. }
            | Self::CreateWorkspace { .. }
            | Self::CreateChild { .. }
            | Self::CreateCommitWorkspace { .. }
            | Self::OpenPullHead { .. }
            | Self::AdoptWorkspace { .. }
            | Self::UntrackedWorktrees
            | Self::ListSpaces
            | Self::FindSpaces { .. }
            | Self::LocateSpace { .. }
            | Self::SelectSpaces { .. }
            | Self::VerifyWorkspace { .. }
            | Self::HeldPermissions { .. }
            | Self::IssueScope { .. }
            | Self::RevokeScope { .. }
            | Self::SpaceEnvironment { .. }
            | Self::CopySpace { .. }
            | Self::KeepAgentEnvironment { .. }
            | Self::RecordOwnedPlace { .. }
            | Self::OwnedPlaces
            | Self::CloseOwnedPlace { .. }
            | Self::PlaceOwnership { .. }
            | Self::SpacePlaces { .. }
            | Self::SpaceSessions { .. }
            | Self::CreatePendingWorktree { .. }
            | Self::WorkspaceHook { .. }
            | Self::WatchSpaceEvents { .. } => Forwarding::Never,
        }
    }

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::ReopenWorkspace { workspace }
            | Self::RenameBranch { workspace, .. }
            | Self::ConvertToChat { workspace, .. }
            | Self::SetBaseWorkspace { workspace, .. }
            | Self::InspectWorkspace { workspace }
            | Self::VerifyWorkspace { workspace, .. }
            | Self::FastForwardWorkspace { workspace, .. }
            | Self::RefreshWorkspace { workspace }
            | Self::LaunchPermissions { workspace, .. }
            | Self::DiffBase { workspace }
            | Self::Conflicts { workspace, .. }
            | Self::Diff { workspace, .. }
            | Self::PreviewMerge { workspace, .. }
            | Self::KeepAgentEnvironment { workspace, .. }
            | Self::RecordOwnedPlace { workspace, .. }
            | Self::CreatePendingWorktree { workspace }
            | Self::WorkspaceHook { workspace, .. } => Some(Target::Space(workspace)),
            Self::RenameSpace { space, .. }
            | Self::SpacePlaces { space }
            | Self::IssueScope { space }
            | Self::RevokeScope { space, .. }
            | Self::SpaceEnvironment { space }
            | Self::SpaceStatus { space }
            | Self::TrackedConfig { space }
            | Self::SpaceSessions { space }
            | Self::FollowSession { space, .. }
            | Self::AttachSession { space, .. }
            | Self::CopySpace { space, .. }
            | Self::PlaceOwnership { space, .. } => Some(Target::Space(space)),
            Self::LayeredConfig {
                target: ConfigTarget::Space(space),
            } => Some(Target::Space(space)),
            Self::SpaceLog { space, .. } => space.as_ref().map(Target::Space),
            // A location is this machine's view of where a space runs.
            Self::LocateSpace { .. } | Self::SelectSpaces { .. } => None,
            // A creation names the space it goes below, which a start below
            // another machine's space takes where the caller runs.
            Self::OpenBranch { .. }
            | Self::CreateWorkspace { .. }
            | Self::CreateChild { .. }
            | Self::CreateCommitWorkspace { .. }
            | Self::OpenPullHead { .. }
            | Self::AdoptWorkspace { .. }
            | Self::LayeredConfig { .. }
            | Self::UntrackedWorktrees
            | Self::ListSpaces
            | Self::FindSpaces { .. }
            | Self::HeldPermissions { .. }
            | Self::OwnedPlaces
            | Self::CloseOwnedPlace { .. }
            | Self::WatchSpaceEvents { .. } => None,
        }
    }
}
