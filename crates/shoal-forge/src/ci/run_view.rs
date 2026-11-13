//! Reading CI workflow runs: one run with its jobs and their logs, or the
//! newest runs of a branch or workflow, as the workspace's agent account.
use anyhow::{Context, Result, bail, ensure};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use shoal_refs::CommitId;

use crate::{
    ForgeRepo,
    account::Account,
    backends::{Checks, Runs},
    capability::Capability,
    ci::{JobState, checks::CheckLog, run_action::Run},
    numbers::JobId,
    transport::api::Request,
    updates::{FailedJob, MAX_LISTED_JOBS, Snapshot, SnapshotKind, SnapshotState},
};

/// The most runs one list returns.
pub const MAX_RUNS: usize = 100;
/// How many of the newest runs of a workflow on a branch a lookup of the
/// newest finished one reads.
const FINISHED_LOOKUP: usize = 20;
/// The jobs one page of a run's jobs holds.
pub(crate) const JOBS_PAGE: usize = 100;
/// How many listed failed runs have their jobs read at once.
const JOB_READS: usize = 8;

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

/// Refuse a filter no forge lists runs by, before any lookup.
pub fn validate_filter(filter: &RunFilter) -> Result<()> {
    ensure!(
        (1..=MAX_RUNS).contains(&filter.limit),
        "limit must be between 1 and {MAX_RUNS}"
    );
    for (field, value) in [
        ("branch", filter.branch.as_deref()),
        ("workflow", filter.workflow.as_deref()),
        ("commit", filter.commit.as_ref().map(CommitId::as_str)),
    ] {
        ensure!(
            value.is_none_or(|value| !value.trim().is_empty()),
            "{field} must be nonempty"
        );
    }
    Ok(())
}

/// The job `job` selects among `run`'s `jobs`.
fn find_job<'a>(run: &Run, jobs: &'a [RunJob], job: &JobRef) -> Result<&'a RunJob> {
    let job = &numbered_job(jobs, job);
    let matching = jobs
        .iter()
        .filter(|candidate| match job {
            JobRef::Id(id) => candidate.id == *id,
            JobRef::Name(name) => candidate.name == *name,
        })
        .collect::<Vec<_>>();
    match (job, matching.as_slice()) {
        (_, [found]) => Ok(found),
        (JobRef::Name(name), []) => bail!(
            "run {} has no job {name}; shoal view ci {} lists its jobs' names and IDs",
            run.url,
            run.url
        ),
        (JobRef::Name(name), _) => bail!(
            "several jobs of {} are named {name}; select one by its ID",
            run.url
        ),
        (JobRef::Id(id), _) => bail!("run {} has no job with ID {id}", run.url),
    }
}

/// `job`, or the ID a name that no job of `jobs` has spells.
fn numbered_job(jobs: &[RunJob], job: &JobRef) -> JobRef {
    match job {
        JobRef::Name(name) if !jobs.iter().any(|candidate| candidate.name == *name) => {
            name.parse().map_or_else(|_| job.clone(), JobRef::Id)
        }
        _ => job.clone(),
    }
}

impl ForgeRepo {
    /// The run `run`.
    pub async fn view_run(
        &self,
        account: &Account,
        run: &crate::items::reference::ItemRef,
    ) -> Result<Run> {
        self.require(Capability::ReadRuns)?;
        self.read_run(account, run).await
    }

    /// The jobs of the run `run`.
    pub async fn run_jobs(
        &self,
        account: &Account,
        run: &crate::items::reference::ItemRef,
    ) -> Result<Vec<RunJob>> {
        let run = self.view_run(account, run).await?;
        self.jobs_of(account, &run).await
    }

    /// The whole log of the job `job` of the run `run`.
    pub async fn run_job_log(
        &self,
        account: &Account,
        run: &crate::items::reference::ItemRef,
        job: &JobRef,
    ) -> Result<CheckLog> {
        self.require(Capability::ReadJobLogs)?;
        let run = self.view_run(account, run).await?;
        let jobs = self.jobs_of(account, &run).await?;
        let job = find_job(&run, &jobs, job)?;
        let log = self.kind.run_job_log(self, account, &run, job).await?;
        let step = match self.kind.listed_failed_step(self, account, job.id).await {
            None => self.kind.failed_step(&log),
            listed => listed,
        };
        let log = CheckLog::whole(&run.url, &job.name, log, step);
        Ok(match &job.url {
            Some(url) => log.with_job(url.clone()),
            None => log,
        })
    }

    /// The activity of the run `run`: its state and, once it failed, its
    /// failed jobs with the steps that failed where their logs say.
    pub async fn run_activity(
        &self,
        account: &Account,
        run: &crate::items::reference::ItemRef,
    ) -> Result<Snapshot> {
        let run = self.view_run(account, run).await?;
        // A run held for approval has yet to run, whatever state the forge
        // gives it, so its watch goes on.
        let state = match run.waiting {
            true => SnapshotState::Waiting,
            false => run.state.into(),
        };
        let mut snapshot = Snapshot::of(SnapshotKind::Run, Some(state));
        if state == SnapshotState::Failed {
            let jobs = self.jobs_of(account, &run).await?;
            snapshot.state = Some(stopped_run_state(&jobs).into());
            snapshot.failed_jobs = self.failed_jobs(account, &run, &jobs).await;
        }
        Ok(snapshot)
    }

    /// The failed jobs of `run` among its `jobs`, with the failing steps of
    /// those a run's update names; only their logs are read.
    async fn failed_jobs(&self, account: &Account, run: &Run, jobs: &[RunJob]) -> Vec<FailedJob> {
        let failed = jobs.iter().filter(|job| job.state == JobState::Failed);
        let mut failed_jobs = Vec::new();
        for (index, job) in failed.enumerate() {
            let step = match index < MAX_LISTED_JOBS {
                true => self.failed_job_step(account, run, job).await,
                false => None,
            };
            failed_jobs.push(FailedJob {
                name: job.name.clone(),
                step,
            });
        }
        failed_jobs
    }

    /// The step that failed in `job`, where the forge lists it or reads its
    /// log and the log names one.
    async fn failed_job_step(&self, account: &Account, run: &Run, job: &RunJob) -> Option<String> {
        let listed = self.kind.listed_failed_step(self, account, job.id).await;
        if listed.is_some() || !self.supports(Capability::ReadJobLogs) {
            return listed;
        }
        let log = self.kind.run_job_log(self, account, run, job).await.ok()?;
        self.kind.failed_step(&log)
    }

    /// Every job of `run`, reading page after page while pages are full.
    pub(crate) async fn jobs_of(&self, account: &Account, run: &Run) -> Result<Vec<RunJob>> {
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

    /// The newest runs `filter` selects, newest first, each failed one in
    /// the state its jobs show.
    pub async fn list_runs(&self, account: &Account, filter: &RunFilter) -> Result<Vec<Run>> {
        let runs = self.read_runs(account, filter).await?;
        Ok(futures_util::stream::iter(runs)
            .map(|run| self.settled(account, run))
            .buffered(JOB_READS)
            .collect()
            .await)
    }

    /// `run`, once failed, in the state its jobs show; one whose jobs cannot
    /// be read stays failed.
    async fn settled(&self, account: &Account, run: Run) -> Run {
        if run.state != JobState::Failed {
            return run;
        }
        match self.jobs_of(account, &run).await {
            Ok(jobs) => RunView::new(run, jobs).run,
            Err(_) => run,
        }
    }

    /// The newest runs `filter` selects, newest first, as the forge lists
    /// them.
    async fn read_runs(&self, account: &Account, filter: &RunFilter) -> Result<Vec<Run>> {
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

    /// The newest finished run of `workflow` on `branch`, or of the
    /// branch's pipeline on a forge that runs one per branch; none while
    /// every recent run has yet to finish.
    pub async fn newest_finished_run(
        &self,
        account: &Account,
        workflow: Option<&str>,
        branch: &str,
    ) -> Result<Option<Run>> {
        let filter = RunFilter {
            branch: Some(branch.to_owned()),
            workflow: workflow.map(str::to_owned),
            commit: None,
            limit: FINISHED_LOOKUP,
        };
        Ok(newest_finished(self.read_runs(account, &filter).await?))
    }
}

/// The state of a run the forge reports as failed, from its `jobs`: one
/// whose jobs only stopped short by being cancelled was cancelled.
fn stopped_run_state(jobs: &[RunJob]) -> JobState {
    let failed = jobs.iter().any(|job| job.state == JobState::Failed);
    let cancelled = jobs.iter().any(|job| job.state == JobState::Cancelled);
    match (failed, cancelled) {
        (false, true) => JobState::Cancelled,
        _ => JobState::Failed,
    }
}

/// The first finished run of `runs`, newest first; a run held for approval
/// has yet to run.
fn newest_finished(runs: Vec<Run>) -> Option<Run> {
    runs.into_iter()
        .find(|run| !run.waiting && !run.state.unfinished())
}

#[cfg(test)]
mod tests;
