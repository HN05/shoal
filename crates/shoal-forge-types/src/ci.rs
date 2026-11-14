//! CI jobs and the actions Shoal takes on them: a job's state and the rerun
//! or cancel a caller asks for.
pub mod approval;
pub mod check_state;
pub mod checks;
pub mod run;
pub mod run_action;
pub mod run_view;
pub mod runner;

use serde::{Deserialize, Serialize};
use shoal_states::states;

use crate::ForgeRepo;
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
    /// Whether the action applies to a job in a state, and those states.
    pub fn applies_to(self) -> (fn(JobState) -> bool, &'static str) {
        match self {
            Self::Rerun => (JobState::rerunnable, "failed"),
            Self::Cancel => (JobState::unfinished, "running or waiting"),
        }
    }
}

impl ForgeRepo {
    /// The path components of `target` after this repository's `section`.
    /// Forges link jobs over HTTPS even when the remote uses HTTP.
    pub fn repository_path<'a>(&self, target: &'a str, section: &str) -> Option<Vec<&'a str>> {
        let path = target
            .strip_prefix("https://")
            .or_else(|| target.strip_prefix("http://"))?
            .strip_prefix(&format!("{}/{}", self.host, self.path))?
            .strip_prefix(section)?;
        Some(path.split('/').collect())
    }
}

/// A positive ID in a job URL.
pub fn id(text: &str) -> Option<u64> {
    text.parse::<u64>().ok().filter(|id| *id > 0)
}
