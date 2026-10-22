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
    Restoring,
    Failed,
    Stopped,
    NeedsInput,
    Ready,
    Working,
    Waiting,
    Running,
    ReadyOutdated,
    Inactive,
    Queued,
    Starting,
}

impl ListStatus {
    /// The workspace's state unless ready, then stopped work, an agent
    /// that needs input, a current ready mark, the agent's other turn
    /// states, a running agent or command, an outdated mark, or inactive.
    pub fn of_workspace(workspace: &Workspace, stopped: bool) -> Self {
        let outdated = workspace.review.iter().any(|mark| mark.stale == Some(true));
        let agent = workspace.agent_state.map(|status| status.state);
        match workspace.state {
            WorkspaceState::Preparing => Self::Preparing,
            WorkspaceState::Stopping => Self::Stopping,
            WorkspaceState::Removing => Self::Removing,
            WorkspaceState::Reconciling => Self::Reconciling,
            WorkspaceState::Restoring => Self::Restoring,
            WorkspaceState::Failed => Self::Failed,
            WorkspaceState::Ready if stopped => Self::Stopped,
            WorkspaceState::Ready if agent == Some(AgentState::Waiting) => Self::NeedsInput,
            WorkspaceState::Ready if !workspace.review.is_empty() && !outdated => Self::Ready,
            WorkspaceState::Ready => match agent {
                Some(state) => Self::agent(state),
                None if workspace.running => Self::Running,
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
            Self::Failed => "failed",
            Self::Stopped => "stopped",
            Self::NeedsInput => "needs input",
            Self::Ready => "ready",
            Self::Working => "working",
            Self::Waiting => "waiting",
            Self::Running => "running",
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
            Self::Working | Self::Waiting | Self::Running => Style::Heading,
            Self::Inactive | Self::Queued => Style::Muted,
            Self::Preparing
            | Self::Stopping
            | Self::Removing
            | Self::Reconciling
            | Self::Restoring
            | Self::Stopped
            | Self::NeedsInput
            | Self::ReadyOutdated
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
