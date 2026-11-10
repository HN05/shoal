//! Workspace records and the commit, copy or branch they start from.
use super::{AgentStatus, QueuedJob, ReviewMark, SpaceLinks, SpaceRef, WorkspaceHold};
use crate::repository_ids::RepositoryId;
use crate::space_ids::SpaceId;
use crate::space_names::SpaceName;
use crate::state::{AgentState, WorkspaceState};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub id: SpaceId,
    /// Unique among the spaces that exist, workspaces and chats; freed when
    /// it goes.
    #[serde(default)]
    pub number: u32,
    pub repository_id: RepositoryId,
    pub name: SpaceName,
    /// What its agent calls the work, which its tabs show.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The mode its latest agent started in; none before it had one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<crate::config::modes::ModeName>,
    /// The permissions `--permission` gave its latest agent, which apply as
    /// given whatever its mode's.
    #[serde(
        default,
        skip_serializing_if = "crate::config::permissions::PermissionsConfig::is_empty"
    )]
    pub narrowing: crate::config::permissions::PermissionsConfig,
    pub path: PathBuf,
    /// Its branch; `None` for a workspace at a commit, which `at` describes.
    pub branch: Option<String>,
    /// Where a workspace without a branch was created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<StartingCommit>,
    /// What a copy of another workspace started from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copied_from: Option<CopiedFrom>,
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
    /// Its newest local review.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_review: Option<crate::reviews::ReviewStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_state: Option<AgentStatus>,
    /// Where its agent is in its work, as it last reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<crate::progress::Progress>,
    /// In a list, the turn state its coordinator's agent reports while no
    /// agent of its own has started here, so that agent does its work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator_state: Option<AgentState>,
    #[serde(default)]
    pub links: SpaceLinks,
    /// Whether an agent or command started through Shoal is running here.
    #[serde(default)]
    pub running: bool,
    /// Whether `shoal done` recorded a completion that is not withdrawn.
    #[serde(default)]
    pub done: bool,
    /// Whether it was created without setup, which `shoal work setup` runs.
    #[serde(default)]
    pub setup_skipped: bool,
    /// Whether every removal deletes it whatever it holds, keeping no
    /// checkpoint.
    #[serde(default)]
    pub disposable: bool,
    /// The workspace whose branch this one's branch builds on.
    #[serde(default)]
    pub base_workspace: Option<WorkspaceRef>,
    /// Workspaces that record this one as their base.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stacked_workspaces: Vec<WorkspaceRef>,
    /// The space above this one: the workspace it was split from or the
    /// chat that opened it.
    #[serde(default)]
    pub parent: Option<SpaceRef>,
    /// The spaces below this one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<SpaceRef>,
    /// What its processes use, while any run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<WorkspaceUsage>,
    /// The CI job the runner runs here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<Box<QueuedJob>>,
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

/// The commit a workspace without a branch started at, and what named it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartingCommit {
    pub commit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub made_for: Option<MadeFor>,
}

impl StartingCommit {
    /// The commit as `3f2a9c1`, or `3f2a9c1 (v1.4.0)` with what it was made
    /// for.
    pub fn label(&self) -> String {
        let short = &self.commit[..self.commit.len().min(7)];
        match &self.made_for {
            Some(made_for) => format!("{short} ({made_for})"),
            None => short.to_owned(),
        }
    }
}

/// The workspace a copy was made from and the copy's starting checkpoint,
/// which keeps what it started with for as long as the copy exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopiedFrom {
    /// The workspace copied; `None` once it is removed.
    pub workspace: Option<WorkspaceRef>,
    /// That workspace's checkpoint the copy started from; `None` for a copy
    /// of its files at the time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<u64>,
    /// The number of the copy's own checkpoint of what it started with.
    pub starting_checkpoint: u64,
    /// That checkpoint's HEAD, and the commit that keeps its files.
    pub head: String,
    pub commit: String,
    /// The tree of the index it started with, which the checkpoint does not
    /// keep.
    #[serde(default)]
    pub index: String,
}

impl CopiedFrom {
    /// Whether the copy took its source's files, ignored ones included,
    /// rather than a checkpoint's.
    pub fn took_files(&self) -> bool {
        self.checkpoint.is_none()
    }
}

/// What a workspace at a commit was created for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MadeFor {
    /// A branch or tag, by the name it was given.
    Ref(String),
    /// A PR's head, by its number.
    Pr(u64),
}

impl std::fmt::Display for MadeFor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ref(name) => f.write_str(name),
            Self::Pr(number) => write!(f, "PR #{number}"),
        }
    }
}

/// Another workspace a record refers to, with its current name and branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRef {
    pub id: SpaceId,
    pub name: String,
    pub branch: Option<String>,
}

impl Workspace {
    pub fn new_record(
        repository_id: RepositoryId,
        name: SpaceName,
        path: PathBuf,
        branch: Option<String>,
        state: WorkspaceState,
    ) -> Self {
        Self {
            narrowing: Default::default(),
            id: SpaceId::generate(),
            number: 0,
            repository_id,
            name,
            title: None,
            mode: None,
            path,
            branch,
            at: None,
            copied_from: None,
            state,
            error: None,
            base_commit: None,
            base_ref: None,
            git_dir: None,
            git_dir_id: None,
            holds: Vec::new(),
            review: Vec::new(),
            latest_review: None,
            agent_state: None,
            progress: None,
            coordinator_state: None,
            base_workspace: None,
            stacked_workspaces: Vec::new(),
            parent: None,
            children: Vec::new(),
            links: SpaceLinks::default(),
            running: false,
            done: false,
            setup_skipped: false,
            disposable: false,
            usage: None,
            job: None,
        }
    }

    /// Its branch, refusing a workspace at a commit for `action`, which
    /// needs one.
    pub fn require_branch(&self, action: &str) -> anyhow::Result<&str> {
        self.branch
            .as_deref()
            .ok_or_else(|| no_branch(&self.name, action))
    }

    /// What names its work where a branch would: its branch, or the name
    /// of a workspace at a commit.
    pub fn branch_or_name(&self) -> &str {
        self.branch.as_deref().unwrap_or(&self.name)
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

/// The refusal of `action` in the workspace `name`, which is at a commit and
/// has no branch.
pub fn no_branch(name: &str, action: &str) -> anyhow::Error {
    anyhow::anyhow!("workspace {name} is at a commit and has no branch; {action} needs a branch")
}

/// A workspace on an existing branch, and whether it was already open.
#[derive(Debug, Serialize, Deserialize)]
pub struct OpenedWorkspace {
    pub workspace: Workspace,
    pub reused: bool,
}

/// A workspace after its title changed, with the title it had before.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetitledWorkspace {
    pub workspace: Workspace,
    pub previous_title: Option<String>,
}

/// A branch a new workspace can take, local or on a remote.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Branch {
    pub name: String,
    pub remote: Option<String>,
}
