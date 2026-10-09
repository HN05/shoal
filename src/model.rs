//! Records shared by the daemon, its SQLite store, and CLI output.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::{
    config::repo::{ConflictPolicy, PortDefinition},
    daemon::resources::ResourceLease,
    process::identity::Identity,
    sim::Simulator,
    state::{ExecutionState, WorkspaceState},
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
    /// URL clone. Reserved once, never moved; `None` until first needed.
    pub workspaces_dir: Option<PathBuf>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RepositoryRemoval {
    pub removed: bool,
    pub repository_id: String,
    pub path: PathBuf,
    pub workspaces_removed: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub id: String,
    pub repository_id: String,
    pub name: String,
    pub path: PathBuf,
    pub branch: String,
    pub state: WorkspaceState,
    pub error: Option<String>,
    pub base_commit: Option<String>,
    pub base_ref: Option<String>,
    /// Git's per-worktree admin directory and its device:inode identity, used
    /// to verify the worktree was not moved or replaced.
    pub git_dir: Option<PathBuf>,
    pub git_dir_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub holds: Vec<WorkspaceHold>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub review: Vec<ReviewMark>,
    /// The workspace whose branch this one's branch builds on.
    #[serde(default)]
    pub base_workspace: Option<WorkspaceRef>,
    /// Workspaces that record this one as their base.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stacked_workspaces: Vec<WorkspaceRef>,
}

/// Another workspace a record refers to, with its current name and branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRef {
    pub id: String,
    pub name: String,
    pub branch: String,
}

/// An agent's statement that linked work is ready for review at a commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewMark {
    /// The linked issue or PR; both are absent when the mark covers the
    /// workspace itself.
    pub kind: Option<crate::forge::link::ItemKind>,
    pub url: Option<String>,
    pub head: String,
    pub created_at: i64,
    /// Whether HEAD has moved since the mark; absent when HEAD was not read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale: Option<bool>,
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
            base_workspace: None,
            stacked_workspaces: Vec::new(),
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
}

/// An assignment completion signal, bound to the workspace's HEAD.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Completion {
    pub head: String,
    pub cleanup: bool,
    pub error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Inspection {
    pub issue: Option<crate::forge::issue::Registration>,
    pub completion: Option<Completion>,
    pub pr_cleanup: Option<crate::forge::pr::Registration>,
    pub workspace: Workspace,
    pub executions: Vec<Execution>,
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
    /// The current state of each watched PR.
    #[serde(default)]
    pub prs: Vec<crate::forge::pr::state::PrStatus>,
}

/// Something a workspace links or holds, for finding that workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceTarget {
    /// An issue or PR number or URL.
    Item {
        kind: crate::forge::link::ItemKind,
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
    pub ports: Vec<PortReservation>,
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
    pub configured: std::collections::BTreeMap<String, PortDefinition>,
    pub on_conflict: ConflictPolicy,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DiffBase {
    pub workspace_id: String,
    pub commit: String,
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
