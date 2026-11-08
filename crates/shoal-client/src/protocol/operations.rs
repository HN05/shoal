use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{CreationOptions, ErrorCode, Lifetime, PullHead};
use crate::space_names::SpaceSelector;
use crate::{
    agent::ModelChoice,
    approvals::{ApprovalRequest, DecisionStatus},
    forge::items::create::PullOptions,
    links::OpenOutcome,
    model::{MergedBranch, QueueEntry, Unmerged, Workspace},
};

/// Requests about operations: work the daemon starts in a worker process of
/// its own, so it continues when its caller goes, and whose progress callers
/// follow.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationMethod {
    /// Start `spec` under the caller's scope; answers with
    /// [`super::Body::OperationStarted`]. The worker runs with `env`, the
    /// caller's environment, or without it with the daemon's.
    StartOperation {
        spec: OperationSpec,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env: Option<BTreeMap<String, String>>,
    },
    /// Long-lived: the progress of operation `id` after its first `after`
    /// events, each as [`super::Body::OperationEvent`], then its end as
    /// [`super::Body::OperationEnded`].
    FollowOperation { id: String, after: u64 },
    /// Interrupt operation `id`'s worker, as an interrupt of the command
    /// that ran its work did.
    CancelOperation { id: String },
    /// Open a workspace in `repository` where `start` says and start
    /// preparing it, as `shoal start` does: refuse an agent that cannot run
    /// there or an issue that is not open before anything is created, assign
    /// the issue, then run setup, link `pr` and queue `agent`, with `env` as
    /// [`Self::StartOperation`] takes it. Answers with
    /// [`super::Body::WorkspaceStarted`].
    StartWorkspace {
        repository: crate::repository::Selector,
        #[serde(flatten)]
        start: WorkspaceStart,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base: Option<String>,
        /// Where the worktree of a workspace on a branch goes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<std::path::PathBuf>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        git_profile: Option<String>,
        /// The space opening the workspace, which it goes below; `None`
        /// gives it to the user.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<SpaceSelector>,
        /// Apply only when the workspace is new.
        #[serde(default, skip_serializing_if = "CreationOptions::is_default")]
        options: CreationOptions,
        /// An issue number or URL of the repository.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        issue: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pr: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<Box<QueuedLaunch>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env: Option<BTreeMap<String, String>>,
    },
    /// Split a child from `workspace` and start preparing it, as `shoal
    /// split` does, with the checks of [`Self::StartWorkspace`]: on the new
    /// branch `name`, or with `copy` on a copy of that workspace, on `name`
    /// or else a branch named after the copied one.
    StartSplit {
        workspace: SpaceSelector,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        copy: Option<super::CopySource>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base: Option<String>,
        #[serde(default, skip_serializing_if = "CreationOptions::is_default")]
        options: CreationOptions,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        issue: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<Box<QueuedLaunch>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env: Option<BTreeMap<String, String>>,
    },
    /// The worker's report of a step of operation `id`, or of its end.
    ReportProgress { id: String, report: Report },
}

impl OperationMethod {
    pub fn lifetime(&self) -> Lifetime {
        match self {
            Self::FollowOperation { .. } => Lifetime::Open,
            _ => Lifetime::Reply,
        }
    }
}

/// A started operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Started {
    pub id: String,
}

/// Where a workspace [`OperationMethod::StartWorkspace`] opens starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceStart {
    /// A new branch of this name, from the request's `base`.
    Name(String),
    /// An existing branch, local or remote, reopening the workspace on it.
    Branch(String),
    /// PR `url`'s head, at a commit, reopening the workspace made for the
    /// PR; it takes no base, path or Git profile.
    PullHead { url: String, head: PullHead },
    /// A copy of another workspace of the repository on a new branch,
    /// `branch` or else one named after the copied one's; it takes no base.
    Copy {
        source: super::CopySource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
    },
    /// The recorded workspace of the repository this selector names,
    /// reopened for a launch in it, never created again; it takes no base,
    /// path, Git profile or parent.
    Workspace(SpaceSelector),
}

/// An operation preparing a workspace a request opened, and the workspace
/// as it began: a new one, or with `reused` one it reopened.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartedWorkspace {
    pub operation: String,
    pub workspace: Box<Workspace>,
    #[serde(default)]
    pub reused: bool,
}

/// What the daemon hands an operation's worker on its standard input.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub id: String,
    pub spec: OperationSpec,
}

/// What an operation does.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OperationSpec {
    Prepare {
        workspace: SpaceSelector,
        #[serde(flatten)]
        steps: Preparation,
    },
    /// Push the workspace's branch, then open its PR or link the open one.
    OpenPull {
        workspace: SpaceSelector,
        options: PullOptions,
    },
    /// Merge the workspace's branch as `options` say.
    Merge {
        workspace: SpaceSelector,
        options: crate::model::MergeOptions,
    },
    /// Undo the latest local merge of the workspace's branch.
    Unmerge { workspace: SpaceSelector },
    /// Run the push the user approved in request `approval`, then let the
    /// daemon decide the request, opening the PR it proposed.
    Push {
        workspace: SpaceSelector,
        approval: String,
        push: crate::approvals::PushSpecification,
    },
}

/// What preparing a workspace runs: its setup when `setup`, then linking
/// `pr`, which only warns when refused, then its post-setup hook when
/// `post_setup`, then queuing `agent`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Preparation {
    pub setup: bool,
    pub post_setup: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<Box<QueuedLaunch>>,
}

/// An agent to queue in a ready workspace, as a `shoal start` without a
/// terminal or a scoped one queues the new workspace's.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QueuedLaunch {
    pub agent: String,
    /// Its first message when no issue gives it one.
    pub prompt: Option<String>,
    /// The mode it starts in; the default mode when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// The `--permission` narrowing of its mode's permissions.
    #[serde(
        default,
        skip_serializing_if = "crate::config::permissions::PermissionsConfig::is_empty"
    )]
    pub narrowing: crate::config::permissions::PermissionsConfig,
    /// The model and effort requested over the configured ones.
    #[serde(default, skip_serializing_if = "ModelChoice::is_empty")]
    pub model: ModelChoice,
    /// Arguments passed on to the agent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// The session the agent starts from a fork of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork: Option<String>,
    /// The issue the agent works on, which prompts it instead of `prompt`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<LaunchIssue>,
    /// The variables that place its start beside the caller's terminal;
    /// without them it starts as one from no terminal does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
    /// Start at once regardless of the limits, which scoped callers cannot.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub now: bool,
}

/// The issue a queued agent works on: its URL, which the start looks up
/// for the prompt, and the title the queue shows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchIssue {
    pub url: String,
    pub title: String,
}

impl OperationSpec {
    /// The workspace the operation works in.
    pub fn workspace_mut(&mut self) -> &mut SpaceSelector {
        match self {
            Self::Prepare { workspace, .. }
            | Self::OpenPull { workspace, .. }
            | Self::Merge { workspace, .. }
            | Self::Unmerge { workspace }
            | Self::Push { workspace, .. } => workspace,
        }
    }
}

crate::state::states!(Step {
    SetupStarted => "setup_started",
    PostSetupStarted => "post_setup_started",
    Ready => "ready",
    PrLinked => "pr_linked",
    AgentQueued => "agent_queued",
    PushStarted => "push_started",
    Pushed => "pushed",
    MergeStarted => "merge_started",
    UnmergeStarted => "unmerge_started",
});

crate::state::states!(Stream {
    Stdout => "stdout",
    Stderr => "stderr",
});

/// What an operation reports while it runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Progress {
    Step {
        step: Step,
    },
    /// A line its processes wrote, without its line ending.
    Output {
        stream: Stream,
        line: String,
    },
}

/// The `index`th event of an operation, from 0.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationEvent {
    pub index: u64,
    #[serde(flatten)]
    pub progress: Progress,
}

/// What the worker reports.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Report {
    Step(Step),
    Ended(Ended),
}

/// How an operation ended.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ended {
    Succeeded(Box<Outcome>),
    Failed(Failure),
}

/// What a successful operation did.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Prepared(Prepared),
    Opened(OpenOutcome),
    Merged(MergedBranch),
    Unmerged(Unmerged),
    Decided(Box<crate::approvals::ApprovalRequest>),
}

/// A prepared workspace, ready, and the agent queued in it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prepared {
    pub workspace: Box<Workspace>,
    pub agent: Option<QueueEntry>,
}

// What each kind of operation returns, read from its outcome.
macro_rules! outcomes {
    ($($variant:ident($payload:ty)),+ $(,)?) => {$(
        impl TryFrom<Outcome> for $payload {
            type Error = anyhow::Error;

            fn try_from(outcome: Outcome) -> anyhow::Result<Self> {
                match outcome {
                    Outcome::$variant(value) => Ok(value),
                    _ => anyhow::bail!("the operation ended with an unexpected outcome"),
                }
            }
        }
    )+};
}

outcomes!(
    Prepared(Prepared),
    Opened(OpenOutcome),
    Merged(MergedBranch),
    Unmerged(Unmerged),
    Decided(Box<crate::approvals::ApprovalRequest>)
);

/// Why an operation failed, as typed as its caller acts on it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum Failure {
    /// The setup command failed; the workspace is kept as failed.
    SetupFailed {
        message: String,
    },
    /// The setup command exited with `exit_code`, but Shoal could not verify
    /// that its processes ended; the workspace is kept.
    SetupUnverified {
        exit_code: i32,
    },
    /// The post-setup hook failed; the ready workspace is kept.
    HookFailed {
        message: String,
    },
    PushFailed {
        message: String,
    },
    /// Merging or unmerging waits for the user's decision on `request`, or
    /// was denied.
    Approval {
        request: Box<ApprovalRequest>,
    },
    /// The daemon refused a request of the operation.
    Refused {
        code: ErrorCode,
        message: String,
    },
    Failed {
        message: String,
    },
}

impl Failure {
    /// The code a caller acts on.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::SetupFailed { .. } | Self::SetupUnverified { .. } => ErrorCode::SetupFailed,
            Self::HookFailed { .. } => ErrorCode::HookFailed,
            Self::PushFailed { .. } => ErrorCode::PushFailed,
            Self::Approval { request } if request.status == DecisionStatus::Denied => {
                ErrorCode::ApprovalDenied
            }
            Self::Refused { code, .. } => code.clone(),
            Self::Approval { .. } | Self::Failed { .. } => ErrorCode::OperationFailed,
        }
    }
}
