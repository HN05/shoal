//! A CI workflow run with its jobs, and what selects the runs and jobs to
//! read.

use serde::{Deserialize, Serialize};
use shoal_refs::CommitId;

use crate::{
    ci::{JobState, checks::CheckLog, run_action::Run},
    numbers::JobId,
};

/// A job of a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunJob {
    /// The ID its API reads the log by, which tells apart jobs of one name.
    pub id: JobId,
    pub name: String,
    pub state: JobState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// A run with its jobs, and the job logs a reader added.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunView {
    #[serde(flatten)]
    pub run: Run,
    pub jobs: Vec<RunJob>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub logs: Vec<CheckLog>,
    /// The logs that could not be read, and why.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

impl RunView {
    /// `run` with its `jobs`, in the state they show.
    pub fn new(mut run: Run, jobs: Vec<RunJob>) -> Self {
        if run.state == JobState::Failed {
            run.state = stopped_run_state(&jobs);
        }
        Self {
            run,
            jobs,
            logs: Vec::new(),
            errors: Vec::new(),
        }
    }
}

/// A job of a run, by its ID or by a name no other job of the run has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobRef {
    Id(JobId),
    /// A name, or the ID beside it in a run's view when no job has that name.
    Name(String),
}

/// The runs a list selects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunFilter {
    pub branch: Option<String>,
    pub workflow: Option<String>,
    /// The commit the runs ran on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<CommitId>,
    pub limit: usize,
}

/// The state of a run the forge reports as failed, from its `jobs`: one
/// whose jobs only stopped short by being cancelled was cancelled.
pub fn stopped_run_state(jobs: &[RunJob]) -> JobState {
    let failed = jobs.iter().any(|job| job.state == JobState::Failed);
    let cancelled = jobs.iter().any(|job| job.state == JobState::Cancelled);
    match (failed, cancelled) {
        (false, true) => JobState::Cancelled,
        _ => JobState::Failed,
    }
}

#[cfg(test)]
mod tests;
