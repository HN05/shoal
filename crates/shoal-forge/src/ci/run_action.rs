//! Rerunning and cancelling one CI workflow run, by its web URL or the
//! number in it, as the workspace's agent account. A PR's checks are acted
//! on through [`crate::ci::CiAction`] on its head instead.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use shoal_refs::CommitId;

use crate::{
    ForgeRepo,
    account::Account,
    backends::Runs,
    capability::{self, Capability},
    ci::{CiAction, JobState, id},
    items::reference::ItemRef,
    numbers::{JobId, PullNumber, RunId, RunNumber},
    transport::api::{HttpMethod, Request},
};

/// A workflow run in the repository, as its forge reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    /// The ID its API acts on.
    #[serde(skip)]
    pub(crate) id: RunId,
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
    /// The forge reports it succeeded, which a finished run that was
    /// skipped or waits for manual action has not.
    #[serde(skip)]
    pub(crate) succeeded: bool,
    /// The forge holds the run until a maintainer approves it.
    #[serde(skip)]
    pub(crate) waiting: bool,
}

impl Run {
    /// Whether it started after `other`: forges number runs in the order
    /// they start.
    pub fn started_after(&self, other: &Run) -> bool {
        self.id > other.id
    }

    /// Whether it finished and succeeded.
    pub fn passed(&self) -> bool {
        !self.waiting && self.succeeded
    }
}

/// The requests that apply an action to a run.
pub(crate) enum RunOperation {
    /// POST to this endpoint under the repository's.
    Post(String),
    /// POST to the endpoint under the repository's that each failed job's ID
    /// gives, one job at a time.
    EachFailedJob(fn(JobId) -> String),
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
    pub(crate) fn run(&self, input: &str) -> Result<(RunNumber, String)> {
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

    /// The web URL of the run numbered `number`, without a job.
    pub(crate) fn run_url(&self, number: RunNumber) -> Result<String> {
        let route = self
            .kind
            .run_route()
            .ok_or_else(|| capability::unsupported(&self.kind, Capability::ReadRuns))?;
        Ok(format!("{}{route}{number}", self.repository_url()))
    }

    /// The run `run`, once the forge is known to apply `action` to runs.
    pub async fn find_run(
        &self,
        account: &Account,
        run: &ItemRef,
        action: CiAction,
    ) -> Result<Run> {
        self.require_on_server(account, Capability::ci(action))
            .await?;
        self.read_run(account, run).await
    }

    /// The run `run`.
    pub(crate) async fn read_run(&self, account: &Account, run: &ItemRef) -> Result<Run> {
        run.belongs_to(self)?;
        let number = run.run()?;
        let read = Request::get(self.repo_endpoint(&self.kind.run_path(number)));
        let value = self.send(account, &read).await?;
        self.kind.run(self, number, &value)
    }

    /// Apply `action` to `run`; `failed` reruns only its failed jobs.
    pub async fn act_on_run(
        &self,
        account: &Account,
        run: &Run,
        action: CiAction,
        failed: bool,
    ) -> Result<RunActed> {
        match self.kind.run_operation(run, action, failed)? {
            RunOperation::Post(endpoint) => self.post(account, &endpoint).await?,
            RunOperation::EachFailedJob(endpoint) => {
                self.rerun_failed_jobs(account, run, endpoint).await?
            }
        }
        Ok(RunActed {
            url: run.url.clone(),
            action,
        })
    }

    /// Rerun each failed job of `run` at the endpoint its ID gives. A rerun
    /// also restarts the jobs that need the rerun one, so the jobs are read
    /// again before each rerun, and only one still failed is rerun.
    async fn rerun_failed_jobs(
        &self,
        account: &Account,
        run: &Run,
        endpoint: fn(JobId) -> String,
    ) -> Result<()> {
        let mut rerun = Vec::new();
        loop {
            let jobs = self.jobs_of(account, run).await?;
            let Some(job) = jobs
                .iter()
                .find(|job| job.state.rerunnable() && !rerun.contains(&job.id))
            else {
                break;
            };
            self.post(account, &endpoint(job.id))
                .await
                .with_context(|| format!("could not rerun job {}", job.name))?;
            rerun.push(job.id);
        }
        ensure!(!rerun.is_empty(), "no job of {} has failed", run.url);
        Ok(())
    }

    /// POST to `endpoint` under the repository's.
    async fn post(&self, account: &Account, endpoint: &str) -> Result<()> {
        let request = Request::new(HttpMethod::Post, self.repo_endpoint(endpoint), None);
        self.send(account, &request).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
