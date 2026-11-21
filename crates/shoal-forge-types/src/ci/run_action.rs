//! A CI workflow run as its forge reports it, and the action that reached
//! it. A PR's checks are acted on through [`crate::ci::CiAction`] on its head
//! instead.

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use shoal_refs::CommitId;

use crate::{
    ForgeRepo,
    backends::Routes,
    capability::{Capability, Unsupported},
    ci::{CiAction, JobState, check_state::CheckState},
    numbers::{PullNumber, RunId, RunNumber},
    repo::locator::id,
};

/// A workflow run in the repository, as its forge reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    /// The ID its API acts on.
    #[serde(skip)]
    pub id: RunId,
    pub url: String,
    /// The branch it ran on, when it ran on one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Its workflow file, where the forge runs several per branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    /// The commit it ran on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<CommitId>,
    /// The PR of this repository it ran for, where the forge says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull: Option<PullNumber>,
    pub state: JobState,
    /// What the forge reports it finished with, once it finished.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<CheckState>,
    /// The forge holds the run until a maintainer approves it.
    #[serde(skip)]
    pub waiting: bool,
    /// What tells the latest attempt's finish from an earlier one's, where
    /// the forge says: when it stopped, or the attempt's number.
    #[serde(skip)]
    pub finish: Option<String>,
}

impl Run {
    /// Whether it started after `other`: forges number runs in the order
    /// they start.
    pub fn started_after(&self, other: &Run) -> bool {
        self.id > other.id
    }

    /// Whether it finished and succeeded.
    pub fn passed(&self) -> bool {
        !self.waiting && self.result == Some(CheckState::Success)
    }
}

/// A run an action reached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunActed {
    pub url: String,
    pub action: CiAction,
}

impl ForgeRepo {
    /// The number of the run `input`, its web URL or the number in it, names,
    /// and the run's web URL without a job.
    pub fn run(&self, input: &str) -> Result<(RunNumber, String)> {
        ensure!(
            id(input).is_some() || input.starts_with("https://") || input.starts_with("http://"),
            "name a CI run by its web URL or the number in it"
        );
        let number = id(input)
            .map(RunNumber::new)
            .or_else(|| self.kind.locate_run(self, input))
            .with_context(|| format!("{input} is not a CI run of {}", self.repository_url()))?;
        Ok((number, self.run_url(number)?))
    }

    /// The web URL of the run numbered `number`, without a job. A forge
    /// whose runs have no web URL is one Shoal reads no runs of.
    pub fn run_url(&self, number: RunNumber) -> Result<String> {
        let route = self.kind.run_route().ok_or(Unsupported {
            forge: self.kind.name(),
            capability: Capability::ReadRuns,
            hint: None,
        })?;
        Ok(format!("{}{route}{number}", self.repository_url()))
    }
}

#[cfg(test)]
mod tests;
