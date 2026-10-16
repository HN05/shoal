//! The status column of workspace and agent queue lists.
use crate::{
    cli::output::Style,
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
    Failed,
    Stopped,
    WaitingForInput,
    ReadyForReview,
    Working,
    TurnFinished,
    Running,
    ReadyForReviewOutdated,
    Idle,
    Queued,
    Starting,
}

impl ListStatus {
    /// The workspace's state unless ready, then stopped work, an agent
    /// waiting for input, a current ready-for-review mark, the agent's other
    /// turn states, a running agent or command, an outdated mark, or idle.
    pub fn of_workspace(workspace: &Workspace, stopped: bool) -> Self {
        let outdated = workspace.review.iter().any(|mark| mark.stale == Some(true));
        let agent = workspace.agent_state.map(|status| status.state);
        match workspace.state {
            WorkspaceState::Preparing => Self::Preparing,
            WorkspaceState::Stopping => Self::Stopping,
            WorkspaceState::Removing => Self::Removing,
            WorkspaceState::Reconciling => Self::Reconciling,
            WorkspaceState::Failed => Self::Failed,
            WorkspaceState::Ready if stopped => Self::Stopped,
            WorkspaceState::Ready if agent == Some(AgentState::Waiting) => Self::WaitingForInput,
            WorkspaceState::Ready if !workspace.review.is_empty() && !outdated => {
                Self::ReadyForReview
            }
            WorkspaceState::Ready => match agent {
                Some(state) => Self::agent(state),
                None if workspace.running => Self::Running,
                None if outdated => Self::ReadyForReviewOutdated,
                None => Self::Idle,
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
            AgentState::Waiting => Self::WaitingForInput,
            AgentState::Idle => Self::TurnFinished,
        }
    }

    /// A finished turn is not called idle, which in lists means nothing runs.
    pub fn label(self) -> &'static str {
        match self {
            Self::Preparing => "preparing",
            Self::Stopping => "stopping",
            Self::Removing => "removing",
            Self::Reconciling => "reconciling",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
            Self::WaitingForInput => "waiting for input",
            Self::ReadyForReview => "ready for review",
            Self::Working => "working",
            Self::TurnFinished => "turn finished",
            Self::Running => "running",
            Self::ReadyForReviewOutdated => "ready for review (outdated)",
            Self::Idle => "idle",
            Self::Queued => "queued",
            Self::Starting => "starting",
        }
    }

    pub fn style(self) -> Style {
        match self {
            Self::Failed => Style::Error,
            Self::ReadyForReview => Style::Success,
            Self::Working | Self::TurnFinished | Self::Running => Style::Heading,
            Self::Idle | Self::Queued => Style::Muted,
            Self::Preparing
            | Self::Stopping
            | Self::Removing
            | Self::Reconciling
            | Self::Stopped
            | Self::WaitingForInput
            | Self::ReadyForReviewOutdated
            | Self::Starting => Style::Warning,
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::ValueEnum;

    use super::*;

    #[test]
    fn value_names_spell_the_labels() {
        for status in ListStatus::value_variants() {
            let name = status.to_possible_value().unwrap();
            assert_eq!(
                name.get_name(),
                status
                    .label()
                    .replace(" (", " ")
                    .replace(')', "")
                    .replace(' ', "-")
            );
        }
    }
}
