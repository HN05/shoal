//! Why automatic cleanup keeps a workspace.
use crate::state::WorkspaceState;
use serde::{Deserialize, Serialize};

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
    /// A push waits for the user's approval.
    PushRequest,
    /// An agent launch in the workspace waits in the agent queue or is starting.
    QueuedAgent,
    /// A chat's agent runs.
    AgentRunning,
    /// A chat runs in the workspace's worktree.
    ChatRunning,
    PrWatch,
    /// An ancestor finished its assignment with cleanup, which takes this
    /// workspace too.
    AncestorCompleted {
        workspace: String,
    },
    Completed {
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        integrated: bool,
    },
    Issue,
    Recovering,
    RunningCommands,
    Permits,
    Simulators,
    /// No agent of its own has started there, and the agent of the
    /// coordinator that does its work runs.
    ParentAgentRunning,
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
            Self::PushRequest => f.write_str("Push waiting for approval"),
            Self::QueuedAgent => f.write_str("Queued agent"),
            Self::AgentRunning => f.write_str("Agent running"),
            Self::ChatRunning => f.write_str("Chat running"),
            Self::PrWatch => f.write_str("Waiting for watched PRs to merge"),
            Self::AncestorCompleted { workspace } => {
                write!(f, "{workspace} done; completion cleanup pending")
            }
            Self::Completed { integrated: false } => {
                f.write_str("Done; completion cleanup pending")
            }
            Self::Completed { integrated: true } => f.write_str("Done; integrated elsewhere"),
            Self::Issue => f.write_str("Linked issue"),
            Self::Recovering => f.write_str("Execution recovery pending"),
            Self::RunningCommands => f.write_str("Tracked commands running"),
            Self::Permits => f.write_str("Resource permits held"),
            Self::Simulators => f.write_str("Simulator leases held"),
            Self::ParentAgentRunning => f.write_str("Parent agent running"),
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
