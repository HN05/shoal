//! Records shared by the daemon, its SQLite store, and CLI output.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::{
    config::repo::{ConflictPolicy, PortDefinition},
    daemon::resources::ResourceLease,
    process::identity::Identity,
    sim::Simulator,
    state::{AgentState, ExecutionState, QueueKind, QueueState, WorkspaceState},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repository {
    pub id: String,
    pub path: PathBuf,
    pub source: String,
    /// Monotonic usage counter; higher means more recently used.
    pub last_used: i64,
    pub name: Option<String>,
    /// `<root_dir>/<name>`: parent of this repository's workspaces and of its
    /// URL clone. Reserved once and moved only into a new `root_dir`; `None`
    /// until first needed.
    pub workspaces_dir: Option<PathBuf>,
    /// Registered by `shoal contribute`: removed once nothing of it is left.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub contribution: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RepositoryRemoval {
    pub removed: bool,
    pub repository_id: String,
    pub path: PathBuf,
    pub workspaces_removed: usize,
    /// Canonical worktree paths removed during this operation and prior retry attempts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_paths: Vec<PathBuf>,
}

/// The repository directories a move would take into `directory`, the
/// canonical directory that holds `root_dir`'s repositories.
#[derive(Debug, Serialize, Deserialize)]
pub struct RepositoryMoves {
    pub directory: PathBuf,
    pub repositories: Vec<Repository>,
    /// The commands and agents running in their workspaces, which must stop
    /// before the move.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub executions: Vec<Execution>,
}

/// A repository directory moved into the configured `root_dir`.
#[derive(Debug, Serialize, Deserialize)]
pub struct MovedRepository {
    /// The registration as recorded after the move.
    pub repository: Repository,
    pub from: PathBuf,
    /// The workspace paths before the move.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub id: String,
    pub repository_id: String,
    pub name: String,
    /// What its agent calls the work, which its tabs show.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub path: PathBuf,
    pub branch: String,
    pub state: WorkspaceState,
    pub error: Option<String>,
    pub base_commit: Option<String>,
    pub base_ref: Option<String>,
    /// Git's per-worktree admin directory and its filesystem identity, used
    /// to verify the worktree was not moved or replaced.
    pub git_dir: Option<PathBuf>,
    pub git_dir_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub holds: Vec<WorkspaceHold>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub review: Vec<ReviewMark>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_state: Option<AgentStatus>,
    #[serde(default)]
    pub links: WorkspaceLinks,
    /// Whether an agent or command started through Shoal is running here.
    #[serde(default)]
    pub running: bool,
    /// Whether `shoal done` recorded a completion that is not withdrawn.
    #[serde(default)]
    pub done: bool,
    /// The workspace whose branch this one's branch builds on.
    #[serde(default)]
    pub base_workspace: Option<WorkspaceRef>,
    /// Workspaces that record this one as their base.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stacked_workspaces: Vec<WorkspaceRef>,
    /// The workspace this one was split from.
    #[serde(default)]
    pub parent_workspace: Option<WorkspaceRef>,
    /// Workspaces split from this one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub child_workspaces: Vec<WorkspaceRef>,
    /// The running chat that opened this workspace.
    #[serde(default)]
    pub chat: Option<String>,
    /// What its processes use, while any run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<WorkspaceUsage>,
}

/// The CPU and memory a workspace's processes use, from the daemon's latest
/// sample of the processes carrying its identity.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceUsage {
    /// Percent of one core used since the previous sample; absent until the
    /// daemon has two.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_percent: Option<f64>,
    pub memory_bytes: u64,
    pub processes: usize,
}

/// Another workspace a record refers to, with its current name and branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRef {
    pub id: String,
    pub name: String,
    pub branch: String,
}

/// Canonical issue and pull-request links associated with a workspace.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceLinks {
    /// The assigned issue.
    pub issue: Option<String>,
    /// The linked issue's title when the linking caller knew it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_title: Option<String>,
    /// The assigned PRs.
    pub prs: Vec<String>,
    /// Issues and PRs the workspace created through Shoal, oldest first.
    #[serde(default)]
    pub opened: Vec<LinkedItem>,
    /// Issues and PRs linked as related to the work, oldest first.
    #[serde(default)]
    pub related: Vec<LinkedItem>,
}

impl WorkspaceLinks {
    /// Every linked item once: the assigned issue and PRs, then those opened
    /// and related.
    pub fn items(&self) -> Vec<LinkedItem> {
        use crate::forge::items::link::LinkKind;
        let issue = self.issue.iter().map(|url| LinkedItem {
            kind: LinkKind::Issue,
            url: url.clone(),
            title: self.issue_title.clone(),
        });
        let prs = self.prs.iter().map(|url| LinkedItem {
            kind: LinkKind::Pr,
            url: url.clone(),
            title: None,
        });
        let recorded = self.opened.iter().chain(&self.related).cloned();
        let mut items: Vec<LinkedItem> = Vec::new();
        for item in issue.chain(prs).chain(recorded) {
            match items.iter_mut().find(|known| known.url == item.url) {
                Some(known) => known.title = known.title.take().or(item.title),
                None => items.push(item),
            }
        }
        items
    }
}

crate::state::states!(
    /// How an issue or PR relates to a workspace.
    Relation: ValueEnum {
        /// The work the workspace exists for; it drives completion and cleanup.
        Assigned => "assigned",
        /// The workspace created the item through Shoal.
        Opened => "opened",
        /// The item informs the work without driving it.
        Related => "related",
    }
);

impl Default for Relation {
    /// Links recorded before relations existed were assignments.
    fn default() -> Self {
        Self::Assigned
    }
}

/// An issue, PR or CI run a workspace records without lifecycle effect.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LinkedItem {
    pub kind: crate::forge::items::link::LinkKind,
    pub url: String,
    /// The item's title when the recording caller knew it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

impl LinkedItem {
    /// Whether the item belongs to the repository `remote` names, as an
    /// issue opened elsewhere does not.
    pub fn in_repository(&self, remote: &str) -> bool {
        crate::forge::items::link::item(self.kind, &self.url)
            .is_ok_and(|(repository, _)| repository.is_remote(remote))
    }
}

crate::state::states!(
    /// Why a checkpoint was taken.
    CheckpointCause {
        Manual => "manual",
        /// An agent exited or was stopped.
        AgentStopped => "agent_stopped",
        /// The state a restore to an earlier checkpoint replaced.
        Restore => "restore",
        Removal => "removal",
    }
);

/// A workspace's HEAD and unsaved files, kept until `expires_at`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub workspace_id: String,
    pub repository_id: String,
    /// Counts the workspace's checkpoints from 1.
    pub number: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub cause: CheckpointCause,
    /// The branch checked out; `None` for a detached HEAD.
    pub branch: Option<String>,
    pub head: String,
    /// HEAD, or a commit on top of it with the unsaved files.
    pub commit: String,
    /// Unsaved files exceeded the archive limit and were not kept.
    pub files_omitted: bool,
    pub created_at: u64,
    pub expires_at: u64,
}

/// A workspace `shoal work restore` returned to one of its checkpoints, and the
/// checkpoint of the state it replaced.
#[derive(Debug, Serialize, Deserialize)]
pub struct RestoredCheckpoint {
    pub workspace: Workspace,
    pub checkpoint: Checkpoint,
    pub replaced: Checkpoint,
}

/// A Git worktree under a repository's Shoal directory that no workspace owns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UntrackedWorktree {
    pub repository_id: String,
    pub path: std::path::PathBuf,
}

/// A branch removal kept because only it has some commits: none of them is on
/// a remote or the default branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptBranch {
    pub repository_id: String,
    /// The repository's name, or its path when it has none.
    pub repository: String,
    pub branch: String,
    /// The commit removal left the branch on.
    pub head: String,
    pub kept_at: u64,
    /// Commits only this branch has.
    pub unpushed_commits: u64,
    pub committed_at: u64,
    pub subject: String,
}

/// A kept branch to delete, at the commit the user saw.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchTarget {
    pub repository_id: String,
    pub branch: String,
    pub head: String,
}

/// What `shoal work restore` recreates of a removed workspace until `expires_at`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovedWorkspace {
    pub workspace_id: String,
    pub repository_id: String,
    pub name: String,
    /// The checkpoint taken at removal.
    pub checkpoint: Checkpoint,
    pub base_ref: Option<String>,
    pub base_commit: Option<String>,
    pub links: WorkspaceLinks,
    /// Agents stopped by the removal or before it, which `shoal resume`
    /// continues once the workspace is restored.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<crate::execution::recovery::StoppedAgent>,
    /// The last sessions of agents, which `shoal resume` continues once the
    /// workspace is restored.
    #[serde(
        default,
        alias = "last_session",
        deserialize_with = "crate::execution::recovery::one_or_many_sessions",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub last_sessions: Vec<crate::execution::recovery::LastSession>,
    pub cause: crate::daemon::events::EventCause,
    pub removed_at: u64,
    pub expires_at: u64,
}

/// A workspace `shoal work restore` opened again from its removal record; the
/// caller sets it up and links its PRs.
#[derive(Debug, Serialize, Deserialize)]
pub struct RestoredWorkspace {
    pub opened: OpenedWorkspace,
    pub removed: RemovedWorkspace,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// An agent's statement that linked work is ready for review at a commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewMark {
    /// The linked issue or PR; both are absent when the mark covers the
    /// workspace itself.
    pub kind: Option<crate::forge::items::link::ItemKind>,
    pub url: Option<String>,
    pub head: String,
    pub created_at: i64,
    /// Whether HEAD has moved since the mark; absent when HEAD was not read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale: Option<bool>,
    /// What the marked item leaves out of the repository's conventions, as
    /// `shoal ready` reports it; never stored.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// The turn state a workspace's agent last reported, and since when.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentStatus {
    pub state: AgentState,
    pub since: i64,
    /// When the prompt cache of an agent idle since a turn ended expires,
    /// in Unix seconds, for an AI tool whose cache lifetime is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache_expires_at: Option<i64>,
}

/// A caller-named claim that a workspace is still in use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceHold {
    pub workspace_id: String,
    pub name: String,
    pub reason: Option<String>,
    pub created_at: i64,
}

impl Workspace {
    pub fn new_record(
        repository_id: String,
        name: String,
        path: PathBuf,
        branch: String,
        state: WorkspaceState,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            repository_id,
            name,
            title: None,
            path,
            branch,
            state,
            error: None,
            base_commit: None,
            base_ref: None,
            git_dir: None,
            git_dir_id: None,
            holds: Vec::new(),
            review: Vec::new(),
            agent_state: None,
            base_workspace: None,
            stacked_workspaces: Vec::new(),
            parent_workspace: None,
            child_workspaces: Vec::new(),
            chat: None,
            links: WorkspaceLinks::default(),
            running: false,
            done: false,
            usage: None,
        }
    }

    /// True when `path` (canonical) lies inside this worktree.
    pub fn contains(&self, path: &Path) -> bool {
        std::fs::canonicalize(&self.path).is_ok_and(|root| path.starts_with(root))
    }

    /// The most deeply nested workspace containing `path`.
    pub fn innermost<'a>(workspaces: &'a [Workspace], path: &Path) -> Option<&'a Workspace> {
        workspaces
            .iter()
            .filter(|w| w.contains(path))
            .max_by_key(|w| w.path.components().count())
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Execution {
    pub id: String,
    pub workspace_id: String,
    pub state: ExecutionState,
    pub wrapper: Option<Identity>,
    pub child: Option<Identity>,
    pub group_id: Option<u32>,
    /// Where it runs in a terminal host, when its wrapper runs in one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<crate::sessions::Origin>,
    /// The agent it runs, when it runs one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

/// A file a completion handed back: the record of Shoal's copy, which tells a
/// changed file from the one handed back before, and its name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandedBack {
    pub id: i64,
    pub name: String,
}

/// An assignment completion signal, bound to the workspace's HEAD.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Completion {
    pub head: String,
    pub cleanup: bool,
    /// Another branch has HEAD's work under other commits, so cleanup need
    /// not wait for them to be pushed.
    #[serde(default)]
    pub integrated: bool,
    /// The files the assignment handed back, which `shoal files` lists with
    /// Shoal's copies.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub results: Vec<HandedBack>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Inspection {
    pub issue: Option<crate::daemon::links::issue::Registration>,
    pub completion: Option<Completion>,
    pub pr_cleanup: Option<crate::daemon::links::pr::Registration>,
    pub workspace: Workspace,
    pub executions: Vec<Execution>,
    /// The places Shoal created for the workspace, such as a session whose
    /// launch failed and left its shell.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub places: Vec<crate::sessions::Origin>,
    pub ports: Vec<PortReservation>,
    pub resources: Vec<ResourceLease>,
    pub simulators: Vec<Simulator>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DiffSummary {
    pub files_changed: u64,
    pub insertions: u64,
    pub deletions: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkspaceStatus {
    #[serde(flatten)]
    pub inspection: Inspection,
    pub setup_finished: bool,
    pub diff: Option<DiffSummary>,
    pub diff_error: Option<String>,
    pub unread_notifications: u64,
    /// Why the daemon's automatic cleanup, which removes this workspace when
    /// it qualifies, is failing or stalled.
    #[serde(default)]
    pub cleanup_error: Option<String>,
    /// Why automatic cleanup keeps this workspace now.
    pub cleanup_reason: CleanupReason,
    /// Unix seconds when automatic cleanup removes the workspace, while its
    /// grace period runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup_at: Option<u64>,
    /// The current state of each watched PR.
    #[serde(default)]
    pub prs: Vec<crate::forge::review::status::PrStatus>,
    /// The linked issue's current state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_status: Option<IssueStatus>,
    /// The retry of the agent's turn an API failure ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_retry: Option<crate::daemon::workspace::turn_retry::TurnRetry>,
    /// The request the agent's proxy waits to retry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_retry: Option<crate::daemon::workspace::proxy_retries::ProxyRetry>,
}

/// The first condition that keeps a workspace from automatic cleanup, in the
/// order the daemon checks them, or the idle delay when none does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum CleanupReason {
    NotReady {
        state: WorkspaceState,
    },
    Held,
    Question,
    /// A PR proposal waits for the user's approval.
    PrProposal,
    /// An agent launch in the workspace waits in the agent queue or is starting.
    QueuedAgent,
    PrWatch,
    Completed {
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        integrated: bool,
    },
    Issue,
    Recovering,
    RunningCommands,
    Permits,
    Simulators,
    MissingDirectory,
    Dirty,
    Unpushed {
        commits: u64,
    },
    DirectoryProcesses {
        processes: Vec<String>,
    },
    IdleDisabled,
    InspectionFailed {
        error: String,
    },
    /// Nothing keeps the workspace; idle cleanup removes it after this delay.
    Idle {
        minutes: u64,
    },
}

impl std::fmt::Display for CleanupReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotReady { state } => write!(f, "Workspace state: {state}"),
            Self::Held => f.write_str("Kept by holds"),
            Self::Question => f.write_str("Open question"),
            Self::PrProposal => f.write_str("PR proposal waiting for approval"),
            Self::QueuedAgent => f.write_str("Queued agent"),
            Self::PrWatch => f.write_str("Waiting for watched PRs to merge"),
            Self::Completed { integrated: false } => {
                f.write_str("Done; completion cleanup pending")
            }
            Self::Completed { integrated: true } => f.write_str("Done; integrated elsewhere"),
            Self::Issue => f.write_str("Linked issue"),
            Self::Recovering => f.write_str("Execution recovery pending"),
            Self::RunningCommands => f.write_str("Tracked commands running"),
            Self::Permits => f.write_str("Resource permits held"),
            Self::Simulators => f.write_str("Simulator leases held"),
            Self::MissingDirectory => f.write_str("Directory missing"),
            Self::Dirty => f.write_str("Uncommitted or untracked changes"),
            Self::Unpushed { commits } => write!(
                f,
                "{commits} {} neither pushed nor on the local default branch",
                if *commits == 1 { "commit" } else { "commits" }
            ),
            Self::DirectoryProcesses { processes } => {
                write!(f, "Processes using the directory: {}", processes.join(", "))
            }
            Self::IdleDisabled => f.write_str("Idle cleanup disabled"),
            Self::InspectionFailed { error } => write!(f, "Inspection failed: {error}"),
            Self::Idle { minutes } => write!(
                f,
                "Removed after {minutes} idle {}",
                if *minutes == 1 { "minute" } else { "minutes" }
            ),
        }
    }
}

/// A failed lookup leaves `state` empty and records its error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueStatus {
    /// `open` or `closed`.
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Something a workspace links or holds, for finding that workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceTarget {
    /// An issue or PR number or URL.
    Item {
        kind: crate::forge::items::link::ItemKind,
        input: String,
    },
    /// A resource pool or member with a lease.
    Resource { name: String },
}

/// Everything the execution wrapper needs to launch a tracked command.
#[derive(Debug, Serialize, Deserialize)]
pub struct ExecutionPlan {
    pub id: String,
    pub workspace: Workspace,
    pub scope_token: String,
    /// Absolute setup command path when the execution runs configured setup.
    pub setup_cmd: Option<PathBuf>,
    /// Copy the main checkout's ignored files before the setup command.
    pub copy_ignored: bool,
    pub ports: Vec<PortReservation>,
    #[serde(default)]
    pub environment: std::collections::BTreeMap<String, std::ffi::OsString>,
    pub land: Option<Box<LandPlan>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortReservation {
    pub workspace_id: String,
    pub name: String,
    pub port: u16,
    pub env_var: String,
    pub reason: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PortSuggestion {
    pub workspace_id: String,
    pub name: String,
    pub requested_port: u16,
    pub suggested_port: u16,
    pub env_var: String,
    pub reason: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PortOverview {
    pub workspace: Workspace,
    pub reserved: Vec<PortReservation>,
}

/// The port configuration a workspace's reservations draw from.
#[derive(Debug, Serialize, Deserialize)]
pub struct PortCatalog {
    pub workspace: Workspace,
    pub start: u16,
    pub end: u16,
    pub on_conflict: ConflictPolicy,
    pub configured: std::collections::BTreeMap<String, PortDefinition>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DiffBase {
    pub workspace_id: String,
    pub commit: String,
}

/// Whether a workspace's committed HEAD merges cleanly into a target.
#[derive(Debug, Serialize, Deserialize)]
pub struct ConflictCheck {
    pub workspace_id: String,
    pub target: String,
    pub target_commit: String,
    pub head: String,
    pub conflicts: bool,
    /// Conflicted paths, empty when the merge is clean.
    pub files: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PulledBranch {
    pub branch: String,
    pub repository_id: String,
    pub previous_commit: String,
    pub commit: String,
    pub updated: bool,
    /// Why a merge source was left as it is instead of refreshed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
}

impl PulledBranch {
    pub fn unchanged(repo: &Repository, branch: &str, commit: String, skipped: String) -> Self {
        Self {
            branch: branch.to_owned(),
            repository_id: repo.id.clone(),
            previous_commit: commit.clone(),
            commit,
            updated: false,
            skipped: Some(skipped),
        }
    }
}

/// A workspace on an existing branch, and whether it was already open.
#[derive(Debug, Serialize, Deserialize)]
pub struct OpenedWorkspace {
    pub workspace: Workspace,
    pub reused: bool,
}

/// A repository's remote fetched and its local default branch fast-forwarded.
#[derive(Debug, Serialize, Deserialize)]
pub struct SyncedRepository {
    pub repository_id: String,
    /// The default branch's upstream remote, absent when it has none.
    pub remote: Option<String>,
    pub default_branch: PulledBranch,
}

/// A workspace branch merged into its repository's default branch.
#[derive(Debug, Serialize, Deserialize)]
pub struct LandedBranch {
    pub workspace_id: String,
    pub repository_id: String,
    pub branch: String,
    pub default_branch: String,
    pub previous_commit: String,
    pub commit: String,
    /// False when the default branch already contained the workspace branch.
    pub updated: bool,
    /// True when the default branch fast-forwarded instead of a merge commit.
    pub fast_forward: bool,
    /// How the default branch was refreshed from its upstream first.
    pub default_refresh: PulledBranch,
    /// The remote the default branch was pushed to after landing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pushed_to: Option<String>,
}

/// A daemon-authorized landing, held under the repository Git gate.
#[derive(Debug, Serialize, Deserialize)]
pub struct LandPlan {
    pub workspace: Workspace,
    pub repo: Repository,
    pub source: String,
    pub checkout: Option<PathBuf>,
    pub merge_temporaries: Vec<PathBuf>,
    pub default_refresh: PulledBranch,
}

/// An agent the user talks with in a repository's registered checkout,
/// outside any workspace, while its process runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chat {
    pub id: String,
    pub repository_id: String,
    /// Unique among workspace names and the names of running chats.
    pub name: String,
    pub agent: String,
    pub pid: u32,
    pub started_at: i64,
    /// Where it runs in a terminal host, when it runs in one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<crate::sessions::Origin>,
    /// Its own directory for temporary files, which goes with it; none in a
    /// repository without a directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmp_dir: Option<std::path::PathBuf>,
}

/// A place Shoal created for a workspace, which closes with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedPlace {
    pub workspace_id: String,
    pub origin: crate::sessions::Origin,
}

/// A connected agent, or an execution whose wrapper is gone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunningAgent {
    pub execution: String,
    pub workspace_id: String,
    /// `None` for an execution whose wrapper is gone, which may have run an agent.
    pub agent: Option<String>,
    /// Whether its wrapper can start its session again.
    pub restorable: bool,
    /// The turn state the agent reported last; unknown after a daemon restart
    /// until it reports again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<crate::state::AgentState>,
    /// Whether an interrupt ended its last turn, which `shoal resume`
    /// continues.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub interrupted: bool,
    /// Where it runs in a terminal host, when its wrapper runs in one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<crate::sessions::Origin>,
    /// The `shoal restart` runs a restart started it again for, including
    /// runs that joined another's restart, while this daemon runs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub restarted_by: Vec<String>,
    /// The `shoal restart` of it in progress.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart: Option<PendingRestart>,
}

/// A `shoal restart` that has not started its agent again yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingRestart {
    /// The run that asked for it, unless a daemon before this one took it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// The other runs whose requests joined it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub joined: Vec<String>,
}

/// The session a workspace's agent reported last, which a new agent can
/// start from a fork of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSession {
    /// Named as `--agent` names it.
    pub agent: String,
    pub session: String,
}

/// An agent launch or restore waiting for, or starting in, a `max_agents` slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueEntry {
    pub id: String,
    pub kind: QueueKind,
    pub repository_id: String,
    /// The workspace name: derived from the planned branch for a launch, the
    /// stopped agent's for a restore.
    pub name: String,
    pub agent: String,
    pub issue_title: Option<String>,
    pub state: QueueState,
    /// The 1-based place among waiting entries; `None` once starting.
    pub position: Option<usize>,
    pub created_at: i64,
    /// The workspace a starting launch opened, or a restore's own.
    pub workspace_id: Option<String>,
    /// The stopped execution a restore restores.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<String>,
    /// The existing workspace a launch in it opens, known when it is queued.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opens: Option<String>,
    /// The work a waiting launch still waits for: workspace and queued launch
    /// names and issue URLs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waiting_for: Vec<String>,
    /// Whether a restore waits for an update for its agent, which stopped
    /// between turns, rather than restoring an interrupted one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub waits_for_update: bool,
    /// The branch of the completed dependency a started launch builds on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
}

impl QueueEntry {
    /// The workspace the entry starts its agent in, once known.
    pub fn workspace(&self) -> Option<&str> {
        self.opens.as_deref().or(self.workspace_id.as_deref())
    }
}

/// An entry `shoal queue move` placed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MovedEntry {
    pub entry: QueueEntry,
    /// The entry is the restore of a running agent the move stopped.
    pub stopped: bool,
}

/// Where `shoal queue move` places an entry among the waiting ones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueuePlace {
    /// The 1-based position, as `shoal queue` numbers them.
    Position(usize),
    /// In front of the entry with this ID.
    Before(String),
    Last,
}

/// Work a queued launch waits for until it is complete.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dependency {
    /// A workspace or a queued launch of the same repository, by name or ID.
    Work(String),
    /// An issue, by URL.
    Issue(String),
}

/// An agent launch `shoal add` asks the daemon to admit or queue. The plan
/// is the CLI's own and stays opaque to the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueRequest {
    pub repository: crate::repository::Selector,
    pub name: String,
    pub agent: String,
    pub issue_title: Option<String>,
    pub plan: String,
    /// Terminal multiplexer variables the start needs to reach the same window,
    /// and the caller's output setting.
    pub env: std::collections::BTreeMap<String, String>,
    /// Start at once regardless of the limits; the agent still takes a slot.
    pub now: bool,
    /// The existing workspace the launch opens, such as a child workspace for
    /// its parent's agent. The daemon then starts the entry itself, outside
    /// the caller's processes, whether or not a limit applies.
    #[serde(default)]
    pub workspace: Option<String>,
    /// The work the launch waits for; it always queues while any remains.
    #[serde(default)]
    pub after: Vec<Dependency>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueAdmission {
    /// Whether the caller starts the agent now.
    pub start: bool,
    /// `None` when no limit applies, so nothing tracks the launch.
    pub entry: Option<QueueEntry>,
    /// Entries waiting, including this one when it waits.
    pub waiting: usize,
}

/// What a queued start runs: the plan the entry was queued with.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedPlan {
    pub entry: QueueEntry,
    pub plan: String,
}

/// The items a view selects and the worktree whose origin and forge login
/// look them up. Lookups run in the CLI: discussion can exceed a protocol frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Selected {
    pub path: PathBuf,
    pub items: Vec<SelectedItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectedItem {
    pub url: String,
    pub kind: crate::forge::items::link::LinkKind,
}

impl Selected {
    /// The kind of the selected item numbered `number` in the repository
    /// `remote` names, when one is selected; an issue and a PR that share the
    /// number are ambiguous.
    pub(crate) fn kind_of(
        &self,
        remote: &str,
        number: u64,
    ) -> anyhow::Result<Option<crate::forge::items::link::LinkKind>> {
        // A number alone never names a run.
        let mut kinds = self
            .items
            .iter()
            .filter(|item| item.kind.item().is_some())
            .filter(|item| {
                crate::forge::items::link::item(item.kind, &item.url)
                    .is_ok_and(|(repository, item)| item == number && repository.is_remote(remote))
            })
            .map(|item| item.kind);
        let kind = kinds.next();
        anyhow::ensure!(
            kinds.all(|other| Some(other) == kind),
            "an issue and a PR #{number} are both linked; put issue or pr before the number"
        );
        Ok(kind)
    }
}

#[cfg(test)]
mod tests;
