//! The status column of workspace and agent queue lists, and the marker
//! that starts each row.
use crate::{
    model::{Chat, QueueEntry, Workspace},
    state::{AgentState, QueueState, WorkspaceState},
};

/// What a list row shows a workspace, chat or queued agent doing. Its value name
/// is the row's label in kebab case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
pub enum ListStatus {
    Preparing,
    Stopping,
    Removing,
    Reconciling,
    Restoring,
    Updating,
    Failed,
    Stopped,
    NeedsInput,
    Finished,
    Ready,
    Working,
    Waiting,
    /// An agent or command without a reported turn state runs.
    ProcessRunning,
    ReadyOutdated,
    Inactive,
    Queued,
    Starting,
}

impl ListStatus {
    /// The workspace's state unless ready, then stopped work, an agent
    /// that needs input, a recorded completion, a current ready mark, the agent's other turn
    /// states, an agent or command running without one, an outdated mark,
    /// or inactive.
    pub fn of_workspace(workspace: &Workspace, stopped: bool) -> Self {
        let outdated = workspace.review.iter().any(|mark| mark.stale == Some(true));
        let agent = workspace.agent_state.map(|status| status.state);
        match workspace.state {
            WorkspaceState::Preparing => Self::Preparing,
            WorkspaceState::Stopping => Self::Stopping,
            WorkspaceState::Removing => Self::Removing,
            WorkspaceState::Reconciling => Self::Reconciling,
            WorkspaceState::Restoring => Self::Restoring,
            WorkspaceState::Updating => Self::Updating,
            WorkspaceState::Failed => Self::Failed,
            WorkspaceState::Ready if stopped => Self::Stopped,
            WorkspaceState::Ready if agent == Some(AgentState::Waiting) => Self::NeedsInput,
            WorkspaceState::Ready if workspace.done => Self::Finished,
            WorkspaceState::Ready if !workspace.review.is_empty() && !outdated => Self::Ready,
            WorkspaceState::Ready => match agent {
                Some(state) => Self::agent(state),
                None if workspace.running => Self::ProcessRunning,
                None if outdated => Self::ReadyOutdated,
                None => Self::Inactive,
            },
        }
    }

    /// A chat is stopped until it is restored; a running one needs input
    /// while its agent does, is finished once it records completion, ready
    /// once it marks its work ready, or else shows its agent's other turn
    /// states, or that it runs without one.
    pub fn of_chat(chat: &Chat) -> Self {
        let agent = chat.agent_state.map(|status| status.state);
        match chat.restore {
            Some(_) => Self::Stopped,
            None if agent == Some(AgentState::Waiting) => Self::NeedsInput,
            None if chat.completion.is_some() => Self::Finished,
            None if !chat.review.is_empty() => Self::Ready,
            None => agent.map_or(Self::ProcessRunning, Self::agent),
        }
    }

    pub fn of_entry(entry: &QueueEntry) -> Self {
        match entry.state {
            QueueState::Queued => Self::Queued,
            QueueState::Starting => Self::Starting,
        }
    }

    pub fn agent(state: AgentState) -> Self {
        match state {
            AgentState::Working => Self::Working,
            AgentState::Waiting => Self::NeedsInput,
            AgentState::Idle => Self::Waiting,
        }
    }

    /// An agent whose turn finished is waiting, for the user or for PR
    /// activity that wakes it; inactive means nothing runs.
    pub fn label(self) -> &'static str {
        match self {
            Self::Preparing => "preparing",
            Self::Stopping => "stopping",
            Self::Removing => "removing",
            Self::Reconciling => "reconciling",
            Self::Restoring => "restoring",
            Self::Updating => "updating",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
            Self::NeedsInput => "needs input",
            Self::Finished => "finished",
            Self::Ready => "ready",
            Self::Working => "working",
            Self::Waiting => "waiting",
            Self::ProcessRunning => "process running",
            Self::ReadyOutdated => "ready (outdated)",
            Self::Inactive => "inactive",
            Self::Queued => "queued",
            Self::Starting => "starting",
        }
    }

    /// What the status asks of the user, which its row marker shows.
    pub fn group(self) -> Group {
        match self {
            Self::NeedsInput | Self::Ready | Self::Stopped => Group::YourTurn,
            Self::Working | Self::ProcessRunning => Group::Busy,
            Self::Waiting
            | Self::Finished
            | Self::ReadyOutdated
            | Self::Inactive
            | Self::Queued => Group::Quiet,
            Self::Preparing
            | Self::Stopping
            | Self::Removing
            | Self::Reconciling
            | Self::Restoring
            | Self::Updating
            | Self::Starting => Group::Changing,
            Self::Failed => Group::Failed,
        }
    }
}

/// Statuses that ask the same of the user share a row marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// The user has something to answer, review or resume.
    YourTurn,
    /// An agent or process works.
    Busy,
    /// Nothing happens until something wakes it.
    Quiet,
    /// The workspace moves between states.
    Changing,
    Failed,
}

impl Group {
    /// The one-column row marker, whose shape stays readable without color.
    pub fn marker(self) -> &'static str {
        match self {
            Self::YourTurn => "●",
            Self::Busy => "◐",
            Self::Quiet => "○",
            Self::Changing => "◌",
            Self::Failed => "✗",
        }
    }
}
