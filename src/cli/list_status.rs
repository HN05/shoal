//! The status column of workspace and agent queue lists, and the marker
//! that starts each row.
use crate::{
    cli::output::{Palette, Style},
    model::{QueueEntry, Workspace},
    state::{AgentState, QueueState, WorkspaceState},
};

/// What a list row shows a workspace or queued agent doing. Its value name
/// is the row's label in kebab case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
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

    pub fn style(self) -> Style {
        match self {
            Self::Failed => Style::Error,
            Self::Ready => Style::Success,
            Self::Finished => Style::Done,
            Self::Working => Style::Heading,
            Self::Waiting => Style::Idle,
            Self::ProcessRunning => Style::Active,
            Self::NeedsInput => Style::Attention,
            Self::Inactive | Self::Queued => Style::Muted,
            Self::Preparing
            | Self::Stopping
            | Self::Removing
            | Self::Reconciling
            | Self::Restoring
            | Self::Updating
            | Self::Stopped
            | Self::ReadyOutdated
            | Self::Starting => Style::Warning,
        }
    }

    /// A one-column marker for what the status asks of the user, whose shape
    /// stays readable without color.
    pub fn marker(self, palette: Palette) -> String {
        let (style, marker) = match self.group() {
            Group::YourTurn => (Style::Attention, "●"),
            Group::Busy => (Style::Heading, "◐"),
            Group::Quiet => (Style::Muted, "○"),
            Group::Changing => (Style::Warning, "◌"),
            Group::Failed => (Style::Error, "✗"),
        };
        palette.paint(style, marker)
    }

    fn group(self) -> Group {
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
enum Group {
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

#[cfg(test)]
mod tests;
