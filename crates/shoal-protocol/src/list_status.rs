//! The status lists show for a workspace, chat or queued agent, which the
//! CLI shows its own spaces with and a machine reports to the hub for each
//! of its spaces.
use crate::{
    model::{Chat, QueueEntry, Workspace},
    progress::Progress,
    state::{AgentState, QueueKind, QueueState, Sleep, WorkspaceState},
};

shoal_states::states! {
    /// What a list row shows a workspace, chat or queued agent doing; its
    /// value name is its label in kebab case. Lists sort spaces by it in
    /// declaration order: working agents first, then what needs the user,
    /// failures, changes under way, queued agents, and quiet spaces last.
    #[derive(PartialOrd, Ord)]
    ListStatus: ValueEnum {
        Working => "working",
        /// An agent or command without a reported turn state runs.
        ProcessRunning => "process-running",
        /// No agent of its own has started, and the agent above that does
        /// its work is working.
        ParentWorking => "parent-working",
        NeedsInput => "needs-input",
        Ready => "ready",
        Stopped => "stopped",
        Failed => "failed",
        Starting => "starting",
        Preparing => "preparing",
        Restoring => "restoring",
        Updating => "updating",
        Reconciling => "reconciling",
        Stopping => "stopping",
        Removing => "removing",
        Queued => "queued",
        /// Its progress records what blocks it.
        Blocked => "blocked",
        Waiting => "waiting",
        /// An agent Shoal stopped and brings back on its own, or an idle
        /// one `shoal sleep` put to sleep.
        Asleep => "asleep",
        Inactive => "inactive",
        Finished => "finished",
    }
}

impl ListStatus {
    /// The workspace's state unless ready, then an agent Shoal put to sleep,
    /// whose saved work comes back on its own, a recorded completion when its
    /// work is stopped, stopped work unless blocked, an agent that needs
    /// input, what blocks it, a recorded
    /// completion, a ready mark while its agent is not working,
    /// the agent's other turn states as its sleep says, an agent or command running without one, the
    /// coordinator that does its work working, or inactive.
    pub fn of_workspace(workspace: &Workspace, stopped: bool) -> Self {
        let agent = workspace.agent_state.map(|status| status.state);
        let blocked = workspace.progress.as_ref().is_some_and(Progress::blocked);
        match workspace.state {
            WorkspaceState::Preparing => Self::Preparing,
            WorkspaceState::Stopping => Self::Stopping,
            WorkspaceState::Removing => Self::Removing,
            WorkspaceState::Reconciling => Self::Reconciling,
            WorkspaceState::Restoring => Self::Restoring,
            WorkspaceState::Updating => Self::Updating,
            WorkspaceState::Failed => Self::Failed,
            WorkspaceState::Ready if workspace.asleep.is_some() => Self::Asleep,
            WorkspaceState::Ready if stopped && workspace.done => Self::Finished,
            WorkspaceState::Ready if stopped && !blocked => Self::Stopped,
            WorkspaceState::Ready if agent == Some(AgentState::Waiting) => Self::NeedsInput,
            WorkspaceState::Ready if blocked => Self::Blocked,
            WorkspaceState::Ready if workspace.done => Self::Finished,
            WorkspaceState::Ready if !workspace.review.is_empty() && marks_wait(agent) => {
                Self::Ready
            }
            WorkspaceState::Ready => match agent {
                Some(state) => Self::sleeping(state, workspace.sleep),
                None if workspace.running => Self::ProcessRunning,
                None if workspace.coordinator_state == Some(AgentState::Working) => {
                    Self::ParentWorking
                }
                None => Self::Inactive,
            },
        }
    }

    /// [`Self::of_workspace`], or while nothing Shoal started runs in the
    /// ready workspace, the status of `entry`, an agent queued to start there.
    pub fn of_listed_workspace(
        workspace: &Workspace,
        stopped: bool,
        entry: Option<&QueueEntry>,
    ) -> Self {
        match entry {
            Some(entry) if workspace.state == WorkspaceState::Ready && !workspace.running => {
                Self::of_entry(entry)
            }
            _ => Self::of_workspace(workspace, stopped),
        }
    }

    /// [`Self::of_listed_workspace`] with the first agent of `queued` that
    /// [`queued_in`] finds in `workspace`.
    pub fn of_queued_workspace(
        workspace: &Workspace,
        stopped: bool,
        queued: &[QueueEntry],
    ) -> Self {
        let entry = queued_in(workspace, queued).into_iter().next();
        Self::of_listed_workspace(workspace, stopped, entry)
    }

    /// A chat is asleep while Shoal brings its agent back on its own; else
    /// it is stopped until it is restored, finished when it recorded
    /// completion or blocked while its progress records what blocks it; a
    /// running one needs input while its agent does, is blocked, finished
    /// once it records completion, ready once it is marked ready while its
    /// agent is not working, or else shows its agent's other turn states as its sleep says, or that
    /// it runs without one.
    pub fn of_chat(chat: &Chat) -> Self {
        let agent = chat.agent_state.map(|status| status.state);
        let blocked = chat.progress.as_ref().is_some_and(Progress::blocked);
        if chat.asleep.is_some() {
            return Self::Asleep;
        }
        match chat.running {
            false if chat.completion.is_some() => Self::Finished,
            false if blocked => Self::Blocked,
            false => Self::Stopped,
            true if agent == Some(AgentState::Waiting) => Self::NeedsInput,
            true if blocked => Self::Blocked,
            true if chat.completion.is_some() => Self::Finished,
            true if !chat.review.is_empty() && marks_wait(agent) => Self::Ready,
            true => agent.map_or(Self::ProcessRunning, |state| {
                Self::sleeping(state, chat.sleep)
            }),
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

    /// The agent's turn `state`, an idle agent shown asleep while `sleep`
    /// lasts.
    pub fn sleeping(state: AgentState, sleep: Option<Sleep>) -> Self {
        match (state, sleep) {
            (AgentState::Idle, Some(_)) => Self::Asleep,
            (state, _) => Self::agent(state),
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
            Self::Blocked => "blocked",
            Self::Finished => "finished",
            Self::Ready => "ready",
            Self::Working => "working",
            Self::Waiting => "waiting",
            Self::Asleep => "asleep",
            Self::ProcessRunning => "process running",
            Self::ParentWorking => "parent working",
            Self::Inactive => "inactive",
            Self::Queued => "queued",
            Self::Starting => "starting",
        }
    }

    /// What the status asks of the user, which its row marker shows.
    pub fn group(self) -> Group {
        match self {
            Self::NeedsInput | Self::Ready | Self::Stopped => Group::YourTurn,
            Self::Working | Self::ProcessRunning | Self::ParentWorking => Group::Busy,
            Self::Waiting
            | Self::Asleep
            | Self::Blocked
            | Self::Finished
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

/// The agents of `queued` waiting to start in `workspace`, in queue order,
/// except the restore that brings back its asleep agent, which its own
/// status shows.
pub fn queued_in<'a>(workspace: &Workspace, queued: &'a [QueueEntry]) -> Vec<&'a QueueEntry> {
    let asleep = |entry: &QueueEntry| {
        workspace.asleep.is_some()
            && entry.kind == QueueKind::Restore
            && entry.state == QueueState::Queued
    };
    (queued.iter())
        .filter(|entry| entry.space() == Some(&workspace.id) && !asleep(entry))
        .collect()
}

/// Whether the ready marks of a space whose agent reports `agent` wait for
/// the user: a working agent never does, so its marks show again once its
/// turn ends.
pub fn marks_wait(agent: Option<AgentState>) -> bool {
    agent != Some(AgentState::Working)
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

#[cfg(test)]
mod tests;
