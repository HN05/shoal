use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{CreationOptions, ErrorCode, Forwarding, Lifetime, PullHead};
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
    /// Long-lived: the progress of `operation` after its first `after`
    /// events, each as [`super::Body::OperationEvent`], then its end as
    /// [`super::Body::OperationEnded`].
    FollowOperation { operation: OperationRef, after: u64 },
    /// Interrupt the worker of `operation`, as an interrupt of the command
    /// that ran its work did.
    CancelOperation { operation: OperationRef },
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
        issue: Option<crate::forge::items::reference::ItemInput>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pr: Option<crate::forge::items::reference::ItemInput>,
        /// The agent its preparation queues, on `issue` unless it names a
        /// task of its own.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<Box<LaunchRequest>>,
        /// The agent the caller starts itself once setup succeeds, which
        /// waits in the queue for a retried setup should setup fail.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        waiting: Option<Box<QueuedLaunch>>,
        /// What `agent` waits for besides the preparation. A new
        /// workspace's preparation waits for it too, then creates the
        /// worktree on the branch of the done blocker that holds the
        /// others' work.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        blockers: Vec<crate::progress::Blocker>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env: Option<BTreeMap<String, String>>,
        /// The starting queue entry whose launch opens the workspace. One a
        /// scoped process queued opens it in that process's scope.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        queue_entry: Option<shoal_ids::queue_entry_ids::QueueEntryId>,
    },
    /// Create the workspace `launch` describes, as `shoal create` does,
    /// starting no agent there: placed as a launch's new workspace is, from
    /// the repository, issue, PR or branch link, branch or copy it names,
    /// or the existing workspace that has the branch it names, reopened,
    /// then prepared with `env` as [`Self::StartWorkspace`] prepares it.
    /// A new workspace goes below the space the launch names, else below
    /// `below`, the space the creation was made in. A launch that names an
    /// agent, a mode or anything else only an agent takes is refused.
    /// Answers with [`super::Body::WorkspaceStarted`].
    StartCreation {
        launch: Box<crate::launches::Launch>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        below: Option<SpaceSelector>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env: Option<BTreeMap<String, String>>,
    },
    /// Create a child of the space `parent`, a workspace or a chat, and
    /// start preparing it, as revert's workspace and an agent's worktree
    /// hook do, with the checks of [`Self::StartWorkspace`]: on the new
    /// branch `name`, or with `copy` on a copy of that workspace, on `name`
    /// or else a branch named after the copied one.
    StartChild {
        parent: SpaceSelector,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        copy: Option<super::CopySource>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base: Option<String>,
        #[serde(default, skip_serializing_if = "CreationOptions::is_default")]
        options: CreationOptions,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        issue: Option<crate::forge::items::reference::ItemInput>,
        /// The agent its preparation queues, as for [`Self::StartWorkspace`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<Box<LaunchRequest>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env: Option<BTreeMap<String, String>>,
    },
    /// Turn the chat `chat` into a workspace on `branch`, an existing branch
    /// or a new one from `base`, linked to `issue` when given, and start
    /// preparing it; its agent restarts there in `mode`, `worker` by
    /// default, once its turn ends.
    ConvertToWorkspace {
        chat: SpaceSelector,
        branch: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        issue: Option<crate::forge::items::reference::ItemInput>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base: Option<String>,
        #[serde(default, skip_serializing_if = "CreationOptions::is_default")]
        options: CreationOptions,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<crate::config::modes::ModeName>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env: Option<BTreeMap<String, String>>,
    },
    /// The worker's report of a step of operation `id`, or of its end.
    ReportProgress { id: String, report: Report },
    /// Long-lived: start `order`, a move, under the caller's scope on the
    /// machine of its spaces, answering with
    /// [`super::Body::OperationStarted`], then follow it from its first
    /// event as [`Self::FollowOperation`] does. The worker runs with `env`
    /// as [`Self::StartOperation`] takes it.
    StartMove {
        order: Box<crate::transfer::MoveOrder>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env: Option<BTreeMap<String, String>>,
    },
}

impl OperationMethod {
    pub fn lifetime(&self) -> Lifetime {
        match self {
            Self::FollowOperation { .. } | Self::StartMove { .. } => Lifetime::Open,
            _ => Lifetime::Reply,
        }
    }
}

/// A started operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Started {
    pub id: String,
}

/// An operation a request names: its ID, and the space it works in where
/// the caller knows it, which takes the request to that space's machine
/// when another joined machine runs it, as a start there prepares a
/// workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationRef {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space: Option<SpaceSelector>,
}

impl OperationRef {
    /// Operation `id`, on this machine.
    pub fn here(id: String) -> Self {
        Self { id, space: None }
    }

    /// Operation `id`, which works in `space`, wherever that space runs.
    pub fn in_space(id: String, space: SpaceSelector) -> Self {
        Self {
            id,
            space: Some(space),
        }
    }
}

/// Where a workspace [`OperationMethod::StartWorkspace`] opens starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceStart {
    /// A new branch of this name, from the request's `base`.
    Name(String),
    /// A new branch of exactly this name, as the user typed it, from the
    /// request's `base`; refused rather than suffixed when the name is taken.
    Typed(String),
    /// An existing branch, local or remote, reopening the workspace on it.
    Branch(String),
    /// PR `url`'s head, at a commit, reopening the workspace made for the
    /// PR; it takes no base, path or Git profile.
    PullHead {
        url: crate::forge::items::reference::ItemRef,
        head: PullHead,
    },
    /// A copy of another workspace of the repository on a new branch,
    /// `branch` or else one named after the copied one's; it takes no base.
    Copy {
        source: super::CopySource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
        /// `branch` is the user's own name, which the copy's branch takes
        /// exactly or is refused, as [`Self::Typed`] does.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        typed: bool,
    },
    /// The recorded workspace of the repository this selector names,
    /// reopened for a launch in it, never created again; it takes no base,
    /// path, Git profile or parent.
    Workspace(SpaceSelector),
}

/// An operation preparing a workspace a request opened, and the workspace
/// as it began: a new one, or with `reused` one it reopened, with the queue
/// entry of the agent the request queued there, which waits for the
/// preparation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartedWorkspace {
    /// None while the preparation waits, as the workspace's
    /// `preparation_waits` says, for the daemon to start it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    pub workspace: Box<Workspace>,
    #[serde(default)]
    pub reused: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<Box<crate::model::QueueEntry>>,
    /// What assigning its issue on the forge warns of.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
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
    /// Move spaces to another machine or a file, as
    /// [`OperationMethod::StartMove`] starts it.
    Move {
        order: Box<crate::transfer::MoveOrder>,
    },
    /// Finish the move `record` records, whose worker went before it ended,
    /// as when the machine restarted during it.
    FinishMove {
        record: Box<crate::transfer::MoveRecord>,
    },
}

/// What preparing a workspace runs: creating its worktree when it has none
/// yet, then its setup when `setup`, then linking `pr`, which only warns
/// when refused, then its post-setup hook when `post_setup`, then queuing
/// `agent`. A failed worktree or setup leaves `agent`, or else `waiting`,
/// waiting in the queue for a retried preparation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Preparation {
    pub setup: bool,
    pub post_setup: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<crate::forge::items::reference::ItemInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<Box<QueuedLaunch>>,
    /// The agent the caller starts itself once setup succeeds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting: Option<Box<QueuedLaunch>>,
}

/// An agent to queue in a ready workspace, as a `shoal start` without a
/// terminal or a scoped one queues the new workspace's. A request names it
/// as a [`LaunchRequest`], which the daemon resolves into the launch it
/// queues.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedLaunch<A = crate::agent::Agent> {
    pub agent: A,
    /// Its first message when no issue gives it one.
    pub prompt: Option<String>,
    /// The mode it starts in; the default mode when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<crate::config::modes::ModeName>,
    /// The permissions `--permission` gives its agent, whatever its mode's.
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
    pub fork: Option<shoal_ids::agent_session_ids::AgentSessionId>,
    /// The issue the agent works on, which prompts it instead of `prompt`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<LaunchIssue>,
    /// The variables that place its start beside the caller's terminal;
    /// without them it starts as one from no terminal does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
    /// Start at once regardless of the limits, which a scoped caller does as
    /// its `limits` permission allows.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub now: bool,
    /// The caller runs the agent in its own terminal or process: the queue
    /// leaves the agent's entry to it, rather than start it in a place of
    /// its own, while the caller holds it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub here: bool,
    /// Discard the stopped or asleep agent of the workspace it starts in as
    /// it queues, so it takes that agent's place.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replace: bool,
    /// What made the launch, which its agent's start records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<crate::launches::LaunchSource>,
}

impl<A> QueuedLaunch<A> {
    /// A launch of `agent` that asks for nothing else.
    pub fn new(agent: A) -> Self {
        Self {
            agent,
            prompt: None,
            mode: None,
            narrowing: Default::default(),
            model: ModelChoice::default(),
            args: Vec::new(),
            fork: None,
            issue: None,
            env: None,
            now: false,
            here: false,
            replace: false,
            source: None,
        }
    }
}

impl<A> QueuedLaunch<A> {
    /// Work on `issue` unless the launch names an issue or a task of its
    /// own.
    pub fn default_issue(&mut self, issue: LaunchIssue) {
        if self.issue.is_none() && self.prompt.is_none() && self.args.is_empty() {
            self.issue = Some(issue);
        }
    }

    /// This launch, run by the agent `agent` makes of its own.
    fn map_agent<B>(self, agent: impl FnOnce(A) -> B) -> QueuedLaunch<B> {
        let Self {
            agent: own,
            prompt,
            mode,
            narrowing,
            model,
            args,
            fork,
            issue,
            env,
            now,
            here,
            replace,
            source,
        } = self;
        QueuedLaunch {
            agent: agent(own),
            prompt,
            mode,
            narrowing,
            model,
            args,
            fork,
            issue,
            env,
            now,
            here,
            replace,
            source,
        }
    }
}

/// A launch a client asks for: the agent it names, else the space's
/// `agent.default`.
pub type LaunchRequest = QueuedLaunch<Option<crate::agent::Agent>>;

impl LaunchRequest {
    /// This launch, run by `agent`.
    pub fn run_by(self, agent: crate::agent::Agent) -> QueuedLaunch {
        self.map_agent(|_| agent)
    }
}

impl From<QueuedLaunch> for LaunchRequest {
    fn from(launch: QueuedLaunch) -> Self {
        launch.map_agent(Some)
    }
}

/// The issue a queued agent works on: its URL, which the start looks up
/// for the prompt, the title the queue shows and the labels that may give
/// it a priority.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchIssue {
    pub url: crate::forge::items::reference::ItemRef,
    pub title: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
}

impl OperationSpec {
    /// The workspace the operation works in; none for a move, which works
    /// in its spaces.
    pub fn workspace_mut(&mut self) -> Option<&mut SpaceSelector> {
        match self {
            Self::Prepare { workspace, .. }
            | Self::OpenPull { workspace, .. }
            | Self::Merge { workspace, .. }
            | Self::Unmerge { workspace }
            | Self::Push { workspace, .. } => Some(workspace),
            Self::Move { .. } | Self::FinishMove { .. } => None,
        }
    }

    pub fn kind(&self) -> OperationKind {
        match self {
            Self::Prepare { .. } => OperationKind::Prepare,
            Self::OpenPull { .. } => OperationKind::OpenPull,
            Self::Merge { .. } => OperationKind::Merge,
            Self::Unmerge { .. } => OperationKind::Unmerge,
            Self::Push { .. } => OperationKind::Push,
            Self::Move { .. } => OperationKind::Move,
            Self::FinishMove { .. } => OperationKind::FinishMove,
        }
    }
}

shoal_states::states!(
    /// What an operation does, as its [`OperationSpec`] says.
    OperationKind {
        Prepare => "prepare",
        OpenPull => "open_pull",
        Merge => "merge",
        Unmerge => "unmerge",
        Push => "push",
        Move => "move",
        FinishMove => "finish_move",
    }
);

shoal_states::states!(Step {
    WorktreeStarted => "worktree_started",
    SetupStarted => "setup_started",
    PostSetupStarted => "post_setup_started",
    /// What preparation runs after setup and its hook may start now.
    PostSetupEnded => "post_setup_ended",
    Ready => "ready",
    PrLinked => "pr_linked",
    AgentQueued => "agent_queued",
    PushStarted => "push_started",
    Pushed => "pushed",
    MergeStarted => "merge_started",
    UnmergeStarted => "unmerge_started",
    /// A move stops a space's agents and writes its package.
    MovePacking => "move_packing",
    /// A move hands a package to another machine.
    MoveDelivering => "move_delivering",
    /// A move ends a delivered space here.
    MoveEnding => "move_ending",
});

shoal_states::states!(Stream {
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
    Moved(crate::transfer::MoveReport),
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
    Decided(Box<crate::approvals::ApprovalRequest>),
    Moved(crate::transfer::MoveReport)
);

/// Why an operation failed, as typed as its caller acts on it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum Failure {
    /// Creating the worktree failed; the workspace is kept as failed, with
    /// its worktree still to create.
    WorktreeFailed {
        message: String,
    },
    /// The setup command failed; the workspace is kept as failed.
    SetupFailed {
        message: String,
    },
    /// Setup never ran, as when the daemon refused or did not register it;
    /// the workspace is kept as failed.
    SetupNotStarted {
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
    /// The operation ends what the caller has to consent to first, as
    /// `question` asks; it changed nothing.
    Unconfirmed {
        question: String,
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
    /// What the failure says, where it carries a message.
    pub fn message(&self) -> Option<&str> {
        match self {
            Self::WorktreeFailed { message }
            | Self::SetupFailed { message }
            | Self::SetupNotStarted { message }
            | Self::HookFailed { message }
            | Self::PushFailed { message }
            | Self::Refused { message, .. }
            | Self::Failed { message }
            | Self::Unconfirmed { question: message } => Some(message),
            Self::SetupUnverified { .. } | Self::Approval { .. } => None,
        }
    }

    /// The code a caller acts on.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::WorktreeFailed { .. }
            | Self::SetupFailed { .. }
            | Self::SetupNotStarted { .. }
            | Self::SetupUnverified { .. } => ErrorCode::SetupFailed,
            Self::HookFailed { .. } => ErrorCode::HookFailed,
            Self::PushFailed { .. } => ErrorCode::PushFailed,
            Self::Approval { request } if request.status == DecisionStatus::Denied => {
                ErrorCode::ApprovalDenied
            }
            Self::Refused { code, .. } => code.clone(),
            Self::Unconfirmed { .. } => ErrorCode::Unconfirmed,
            Self::Approval { .. } | Self::Failed { .. } => ErrorCode::OperationFailed,
        }
    }
}

impl OperationMethod {
    /// A conversion goes to the chat's machine, and a move to that of its
    /// spaces, as an operation the caller names with its space goes to that
    /// space's, whose events come on a stream; the rest create spaces where
    /// the caller runs or report the caller's own operations.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::ConvertToWorkspace { .. } | Self::CancelOperation { .. } => Forwarding::Online,
            Self::StartMove { .. } | Self::FollowOperation { .. } => Forwarding::Streamed,
            Self::StartOperation { .. }
            | Self::StartWorkspace { .. }
            | Self::StartCreation { .. }
            | Self::StartChild { .. }
            | Self::ReportProgress { .. } => Forwarding::Never,
        }
    }
}

#[cfg(test)]
mod tests;
