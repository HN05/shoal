//! Reading CI workflow runs: one run with its jobs and their logs, or the
//! newest runs of a branch or workflow, as the workspace's agent account.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    ForgeRepo,
    account::Account,
    backends::Backend,
    capability::Capability,
    ci::{JobState, checks::CheckLog, run_action::Run},
    transport::api::Request,
};

/// The most runs one list returns.
pub const MAX_RUNS: usize = 100;
/// The jobs one page of a run's jobs holds.
pub(crate) const JOBS_PAGE: usize = 100;

/// A job of a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunJob {
    /// The ID its API reads the log by, which tells apart jobs of one name.
    pub id: u64,
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

/// A job of a run, by its ID or by a name no other job of the run has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobRef {
    Id(u64),
    Name(String),
}

/// The runs a list selects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunFilter {
    pub branch: Option<String>,
    pub workflow: Option<String>,
    pub limit: usize,
}

/// Refuse a filter no forge lists runs by, before any lookup.
pub fn validate_filter(filter: &RunFilter) -> Result<()> {
    ensure!(
        (1..=MAX_RUNS).contains(&filter.limit),
        "limit must be between 1 and {MAX_RUNS}"
    );
    for (field, value) in [("branch", &filter.branch), ("workflow", &filter.workflow)] {
        ensure!(
            value.as_ref().is_none_or(|value| !value.trim().is_empty()),
            "{field} must be nonempty"
        );
    }
    Ok(())
}

/// The job `job` selects among `run`'s `jobs`.
fn find_job<'a>(run: &Run, jobs: &'a [RunJob], job: &JobRef) -> Result<&'a RunJob> {
    let matching = jobs
        .iter()
        .filter(|candidate| match job {
            JobRef::Id(id) => candidate.id == *id,
            JobRef::Name(name) => candidate.name == *name,
        })
        .collect::<Vec<_>>();
    match (job, matching.as_slice()) {
        (_, [found]) => Ok(found),
        (JobRef::Name(name), []) => bail!("run {} has no job {name}", run.url),
        (JobRef::Name(name), _) => bail!(
            "several jobs of {} are named {name}; select one by its ID",
            run.url
        ),
        (JobRef::Id(id), _) => bail!("run {} has no job with ID {id}", run.url),
    }
}

impl ForgeRepo {
    /// The run `input`, its web URL or the number in it, names.
    pub async fn view_run(&self, account: &Account, input: &str) -> Result<Run> {
        self.require(Capability::ReadRuns)?;
        self.read_run(account, input).await
    }

    /// The jobs of the run `input` names.
    pub async fn run_jobs(&self, account: &Account, input: &str) -> Result<Vec<RunJob>> {
        let run = self.view_run(account, input).await?;
        self.jobs_of(account, &run).await
    }

    /// The log of the job `job` of the run `input` names.
    pub async fn run_job_log(
        &self,
        account: &Account,
        input: &str,
        job: &JobRef,
    ) -> Result<CheckLog> {
        self.require(Capability::ReadJobLogs)?;
        let run = self.view_run(account, input).await?;
        let jobs = self.jobs_of(account, &run).await?;
        let job = find_job(&run, &jobs, job)?;
        let log = self.kind.run_job_log(self, account, &run, job).await?;
        let log = CheckLog::from_text(&run.url, &job.name, &log);
        Ok(match &job.url {
            Some(url) => log.with_job(url.clone()),
            None => log,
        })
    }

    /// Every job of `run`, reading page after page while pages are full.
    async fn jobs_of(&self, account: &Account, run: &Run) -> Result<Vec<RunJob>> {
        let mut jobs = Vec::new();
        for page in 1.. {
            let Some(path) = self.kind.run_jobs_path(run, page) else {
                break;
            };
            let value = self
                .send(account, &Request::get(self.repo_endpoint(&path)))
                .await?;
            let batch = self.kind.run_jobs(run, jobs.len(), &value)?;
            let full = batch.len() >= JOBS_PAGE;
            jobs.extend(batch);
            if !full {
                break;
            }
        }
        Ok(jobs)
    }

    /// The newest runs `filter` selects, newest first.
    pub async fn list_runs(&self, account: &Account, filter: &RunFilter) -> Result<Vec<Run>> {
        self.require(Capability::ReadRuns)?;
        validate_filter(filter)?;
        let read = Request::get(self.repo_endpoint(&self.kind.runs_path(filter)?));
        let value = self.send(account, &read).await?;
        let runs = self
            .kind
            .listed_runs(&value)
            .context("CI runs are not a list")?;
        runs.iter()
            .take(filter.limit)
            .map(|run| self.kind.run_item(self, run))
            .collect()
    }
}

#[cfg(test)]
mod tests;
