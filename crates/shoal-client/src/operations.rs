//! Operations: work the daemon starts in a process of its own, such as
//! creating a workspace with its setup, so it continues when the client
//! goes. A call that starts one returns its ID; `operation.follow` streams
//! its progress, then its result or the error it failed with.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    methods::QueuedAgent,
    open_enum::open_enum,
    records::{Approval, Workspace},
};

/// An agent to queue in a workspace once it is ready.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AgentToQueue {
    /// The agent to start, such as `claude` or `codex`.
    pub agent: String,
    /// The agent's first message; without one it waits for the user.
    #[serde(default)]
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CreateWorkspace {
    /// The repository's ID, name or path.
    pub repository: String,
    /// The new branch; the workspace takes the name derived from it.
    pub name: String,
    /// What the branch starts from; the default branch when absent.
    #[serde(default)]
    pub base: Option<String>,
    /// The issue the workspace is assigned, by number or URL.
    #[serde(default)]
    pub issue: Option<String>,
    /// The PR the workspace works on, by number or URL, linked once it is ready.
    #[serde(default)]
    pub pr: Option<String>,
    #[serde(default)]
    pub agent: Option<AgentToQueue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SplitWorkspace {
    /// The workspace to split the new one from.
    pub workspace: String,
    /// The child's new branch; the child takes the name derived from it.
    pub name: String,
    /// What the branch starts from; the default branch when absent.
    #[serde(default)]
    pub base: Option<String>,
    /// The issue the child is assigned, by number or URL.
    #[serde(default)]
    pub issue: Option<String>,
    #[serde(default)]
    pub agent: Option<AgentToQueue>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct OpenPr {
    pub workspace: String,
    /// The linked issue's title when absent.
    #[serde(default)]
    pub title: Option<String>,
    /// The description as written; `Closes #<issue>` when absent and an
    /// issue is linked.
    #[serde(default)]
    pub body: Option<String>,
    /// The branch it merges into; the default branch when absent.
    #[serde(default)]
    pub base: Option<String>,
    /// Open it as a draft, or not; the repository's default when absent.
    #[serde(default)]
    pub draft: Option<bool>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub reviewers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LandWorkspace {
    pub workspace: String,
    /// Push the default branch after landing; `[land] push` when absent.
    #[serde(default)]
    pub push: Option<bool>,
}

/// A started operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StartedOperation {
    /// The ID `operation.follow` and `operation.cancel` take.
    pub operation: String,
    /// The workspace it works in, as it is when it starts.
    pub workspace: Workspace,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CancelOperation {
    pub operation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FollowOperation {
    pub operation: String,
    /// Skip the events before this index, as when following again after
    /// reading them.
    #[serde(default)]
    pub after: u64,
}

open_enum! {
    /// A step an operation reached.
    Step {
        /// The workspace's setup command started.
        SetupStarted => "setup_started",
        /// The workspace is ready.
        Ready => "ready",
        /// The post-setup hook started.
        PostSetupStarted => "post_setup_started",
        /// The PR the workspace works on is linked.
        PrLinked => "pr_linked",
        /// The agent to queue is queued.
        AgentQueued => "agent_queued",
        PushStarted => "push_started",
        Pushed => "pushed",
        LandStarted => "land_started",
    }
}

open_enum! {
    /// Where an operation's processes wrote a line.
    OutputStream {
        Stdout => "stdout",
        Stderr => "stderr",
    }
}

/// What an operation reports while it runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Progress {
    Step {
        step: Step,
    },
    /// A line its processes wrote, without its line ending.
    Output {
        stream: OutputStream,
        line: String,
    },
    /// Progress this version does not know; ignore it.
    #[serde(other)]
    Unknown,
}

/// The `index`th event of an operation, from 0.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OperationEvent {
    pub index: u64,
    #[serde(flatten)]
    pub progress: Progress,
}

/// What a successful operation did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OperationResult {
    /// The created or split workspace, ready, and the agent queued in it.
    Prepared {
        workspace: Box<Workspace>,
        agent: Option<QueuedAgent>,
    },
    /// The workspace's PR, opened or found open.
    PrOpened(OpenedPr),
    /// The PR waits for the user's approval before Shoal opens it, as the
    /// repository's `[permissions] pr_open` asks of workspace agents.
    PrProposed {
        approval: Box<Approval>,
    },
    Landed(Landed),
    /// A result this version does not know.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OpenedPr {
    pub url: String,
    pub number: u64,
    /// Whether this call opened it, rather than finding it open.
    pub created: bool,
    /// Whether the workspace links it now.
    pub linked: bool,
    /// What the PR lacks of the repository's conventions.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Landed {
    pub branch: String,
    pub default_branch: String,
    /// The default branch's commit before landing.
    pub previous_commit: String,
    pub commit: String,
    /// False when the default branch already contained the branch.
    pub updated: bool,
    /// Whether the default branch fast-forwarded rather than taking a merge commit.
    pub fast_forward: bool,
    /// The remote the default branch was pushed to.
    pub pushed_to: Option<String>,
}
