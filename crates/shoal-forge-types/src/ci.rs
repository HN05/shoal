//! CI jobs and runs, and the actions Shoal takes on them.
pub mod approval;
pub mod check_state;
pub mod checks;
pub mod run;
pub mod run_action;
pub mod run_view;
pub mod runner;

use serde::{Deserialize, Serialize};
use shoal_states::states;

use crate::capability::Capability;
use crate::ci::check_state::CheckState;

states!(CiAction: ValueEnum {
    Rerun => "rerun",
    Cancel => "cancel",
});

/// The checks an action reached on a PR.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CiActed {
    pub url: String,
    pub action: CiAction,
    pub checks: Vec<String>,
}

states!(
    /// Whether a CI job or run waits for a runner, is running or, once it
    /// finished, whether it failed.
    JobState {
        Failed => "failed",
        Waiting => "waiting",
        Running => "running",
        Finished => "finished",
        /// A job cancelled before it finished, whether or not a runner
        /// took it.
        Cancelled => "cancelled",
    }
);

impl JobState {
    /// Whether the job or run has yet to finish.
    pub fn unfinished(self) -> bool {
        matches!(self, Self::Waiting | Self::Running)
    }

    /// Whether the job stopped without succeeding, so a rerun applies to it.
    pub fn rerunnable(self) -> bool {
        matches!(self, Self::Failed | Self::Cancelled)
    }
}

/// A cancelled check failed to finish, so it can be rerun.
impl From<CheckState> for JobState {
    fn from(state: CheckState) -> Self {
        match state {
            CheckState::Waiting | CheckState::Queued => Self::Waiting,
            state if !state.complete() => Self::Running,
            state if state.failed() || state == CheckState::Cancelled => Self::Failed,
            _ => Self::Finished,
        }
    }
}

impl CiAction {
    /// Which job states the action applies to, and how a message names them.
    pub fn applies_to(self) -> (fn(JobState) -> bool, &'static str) {
        match self {
            Self::Rerun => (JobState::rerunnable, "failed"),
            Self::Cancel => (JobState::unfinished, "running or waiting"),
        }
    }

    /// What a forge needs to take the action.
    pub fn capability(self) -> Capability {
        match self {
            Self::Rerun => Capability::RerunJobs,
            Self::Cancel => Capability::CancelJobs,
        }
    }
}
