//! Reading CI workflow runs: one run with its jobs and their logs, or the
//! newest runs of a branch or workflow, as the workspace's agent account.
use anyhow::{Context, Result, bail, ensure};
use futures_util::StreamExt;
use shoal_refs::CommitId;

use crate::{
    backends::{Checks, Runs},
    capability::CapabilityRequests,
    ci::run_action::RunActionSteps,
    transport::account::Account,
    transport::api::{Request, TransportRequests},
    updates::{FailedJob, MAX_LISTED_JOBS, Snapshot},
};
use shoal_forge_types::updates::{SnapshotKind, SnapshotState};
use shoal_forge_types::{
    ForgeRepo,
    capability::Capability,
    ci::{
        JobState,
        checks::CheckLog,
        run_action::Run,
        run_view::{JobRef, RunFilter, RunJob, RunView, stopped_run_state},
    },
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

/// Reading runs, their jobs and logs.
#[shoal_forge_derive::extension(pub RunViewRequests, pub(crate) RunViewSteps)]
impl ForgeRepo {
    /// The run `run`.
    pub async fn view_run(
        &self,
        account: &Account,
        run: &shoal_forge_types::items::reference::ItemRef,
    ) -> Result<Run> {
        self.require(Capability::ReadRuns)?;
        self.read_run(account, run).await
    }

    /// The jobs of the run `run`.
    pub async fn run_jobs(
        &self,
        account: &Account,
        run: &shoal_forge_types::items::reference::ItemRef,
    ) -> Result<Vec<RunJob>> {
        let run = self.view_run(account, run).await?;
        self.jobs_of(account, &run).await
    }

    /// The whole log of the job `job` of the run `run`.
    pub async fn run_job_log(
        &self,
        account: &Account,
        run: &shoal_forge_types::items::reference::ItemRef,
        job: &JobRef,
    ) -> Result<CheckLog> {
        self.require(Capability::ReadJobLogs)?;
        let run = self.view_run(account, run).await?;
        let jobs = self.jobs_of(account, &run).await?;
        let job = find_job(&run, &jobs, job)?;
        let log = self.kind().run_job_log(self, account, &run, job).await?;
        let step = match self.kind().listed_failed_step(self, account, job.id).await {
            None => self.kind().failed_step(&log),
            listed => listed,
        };
        let log = CheckLog::whole(&run.url, &job.name, log, step);
        Ok(match &job.url {
            Some(url) => log.with_job(url.clone()),
            None => log,
        })
    }

    /// The activity of the run `run`: its state and, once it failed, its
    /// failed jobs with the steps that failed where their logs say. A run
    /// `known` already holds failed at the same finish keeps them from it,
    /// without reading its jobs again.
    pub async fn run_activity(
        &self,
        account: &Account,
        run: &shoal_forge_types::items::reference::ItemRef,
        known: Option<&Snapshot>,
    ) -> Result<Snapshot> {
        let run = self.view_run(account, run).await?;
        // A run held for approval has yet to run, whatever state the forge
        // gives it, so its watch goes on.
        let state = match run.waiting {
            true => SnapshotState::Waiting,
            false => run.state.into(),
        };
        let mut snapshot = Snapshot::of(SnapshotKind::Run, Some(state));
        snapshot.finish = run.finish.clone();
        if state == SnapshotState::Failed {
            match known.filter(|known| snapshot.same_finish(known)) {
                Some(known) => snapshot.keep_failure(known),
                None => self.read_failure(account, &run, &mut snapshot).await?,
            }
        }
        Ok(snapshot)
    }

    /// Record on `snapshot` how the failed `run` stopped and its failed jobs.
    async fn read_failure(
        &self,
        account: &Account,
        run: &Run,
        snapshot: &mut Snapshot,
    ) -> Result<()> {
        let jobs = self.jobs_of(account, run).await?;
        snapshot.state = Some(stopped_run_state(&jobs).into());
        snapshot.failed_jobs = self.failed_jobs(account, run, &jobs).await;
        Ok(())
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
        let listed = self.kind().listed_failed_step(self, account, job.id).await;
        if listed.is_some() || !self.supports(Capability::ReadJobLogs) {
            return listed;
        }
        let log = self
            .kind()
            .run_job_log(self, account, run, job)
            .await
            .ok()?;
        self.kind().failed_step(&log)
    }

    /// Every job of `run`, reading page after page while pages are full.
    pub(crate) async fn jobs_of(&self, account: &Account, run: &Run) -> Result<Vec<RunJob>> {
        let mut jobs = Vec::new();
        for page in 1.. {
            let Some(path) = self.kind().run_jobs_path(run, page) else {
                break;
            };
            let value = self
                .send(account, &Request::get(self.repo_endpoint(&path)))
                .await?;
            let batch = self.kind().run_jobs(run, jobs.len(), &value)?;
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
        let read = Request::get(self.repo_endpoint(&self.kind().runs_path(filter)?));
        let value = self.send(account, &read).await?;
        let runs = self
            .kind()
            .listed_runs(&value)
            .context("CI runs are not a list")?;
        runs.iter()
            .take(filter.limit)
            .map(|run| self.kind().run_item(self, run))
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

/// The first finished run of `runs`, newest first; a run held for approval
/// has yet to run.
fn newest_finished(runs: Vec<Run>) -> Option<Run> {
    runs.into_iter()
        .find(|run| !run.waiting && !run.state.unfinished())
}

#[cfg(test)]
mod tests;
