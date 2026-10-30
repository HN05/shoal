use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{ErrorCode, Lifetime};
use crate::{
    daemon::links::proposals::OpenOutcome,
    forge::items::create::PullOptions,
    model::{QueueEntry, Workspace},
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
        workspace: String,
        #[serde(flatten)]
        steps: Preparation,
    },
    /// Push the workspace's branch, then open its PR or link the open one.
    OpenPull {
        workspace: String,
        options: PullOptions,
    },
}

/// What preparing a workspace runs: its setup when `setup`, then its
/// post-setup hook when `post_setup`, then linking `pr` and queuing `agent`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Preparation {
    pub setup: bool,
    pub post_setup: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<QueuedLaunch>,
}

/// An agent to queue in a ready workspace, with its first message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedLaunch {
    pub agent: String,
    pub prompt: Option<String>,
}

impl OperationSpec {
    /// The workspace the operation works in.
    pub fn workspace_mut(&mut self) -> &mut String {
        match self {
            Self::Prepare { workspace, .. } | Self::OpenPull { workspace, .. } => workspace,
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

outcomes!(Prepared(Prepared), Opened(OpenOutcome));

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
    /// The daemon refused a request of the operation.
    Refused {
        code: ErrorCode,
        message: String,
    },
    Failed {
        message: String,
    },
}
