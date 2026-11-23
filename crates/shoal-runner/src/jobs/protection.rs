//! CI jobs that load protection stops. Shoal ends one as it ends a job
//! whose runner outlived its run, without cancelling its run, which would
//! end the run's other jobs, recording why in the workspace's log. Before
//! anything is signaled it records the job to rerun and has the forge
//! cancel that job alone, which a forge whose runner protocol Shoal speaks
//! does, so the job shows as cancelled rather than failed. Each pass then
//! reruns it on its forge as soon as its run has finished there, whatever
//! this machine's readings, and the forge offers the waiting job to every
//! runner, each Shoal runner admitting it as any other. A rerun its run
//! never allows within [`PATIENCE`] is given up.
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::FutureExt as _;
use rusqlite::params;
use shoal_daemon_base::{
    alerts::{About, NewAlert},
    events::record_job_ended,
};
use shoal_forge::ci::{
    run_action::{JobRerun, RunActionRequests},
    runner::{RunnerRequests, reported_run},
};
use shoal_forge_types::{items::reference::ItemRef, numbers::JobId};
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{alerts::AlertKind, time::UnixTime};

use super::{ClaimedJob, overrun::ended_job};
use crate::{Runner, daemon::Daemon, log};
use shoal_daemon_base::jobs::JobStop;

/// How long after its stop a job waits for its run to allow its rerun.
const PATIENCE: Duration = Duration::from_secs(24 * 60 * 60);

/// How long a job to stop waits for its forge to name the job its runner
/// took, before it stops without a rerun. Only a runner registered for one
/// job asks; another's claim names its job.
const IDENTIFY_LIMIT: Duration = Duration::from_secs(10);

/// How long a job to stop waits for its forge to cancel it, before it
/// stops to fail there instead.
const CANCEL_LIMIT: Duration = Duration::from_secs(10);

/// A job a runner runs, as its forge names it.
struct TakenJob {
    run: ItemRef,
    id: JobId,
    name: String,
}

/// A job protection stopped, waiting to rerun.
struct StoppedJob {
    run: ItemRef,
    id: JobId,
    name: String,
    repository_id: RepositoryId,
    stopped_at: UnixTime,
}

impl Runner {
    /// End the CI job running in the workspace `space` for `pressure`, the
    /// reason load protection stops work, recording it to rerun once its run
    /// has finished, returning whether a job ran there.
    pub async fn stop_job_for_overload(
        &self,
        daemon: &dyn Daemon,
        space: &SpaceId,
        pressure: &'static str,
    ) -> Result<bool> {
        let Some(job) = self.workspace_claim(space).await? else {
            return Ok(false);
        };
        if !self.job_execution_recorded(daemon, &job).await? {
            return Ok(false);
        }
        // Named before the stop, which the forge has a bounded while to do.
        let taken = tokio::time::timeout(IDENTIFY_LIMIT, self.taken_job(daemon, &job))
            .await
            .unwrap_or_else(|_| Err(anyhow::anyhow!("its forge did not answer in time")));
        let (stopped, cancelled) = self
            .interrupt_job(daemon, &job, taken.as_ref().ok())
            .await?;
        let reason = format!("stopped for {pressure}");
        (daemon.stop_job_runner(&job.workspace_id, reason.clone())).await?;
        let (name, run) = match &taken {
            Ok(taken) => (taken.name.clone(), taken.run.to_string()),
            Err(_) => (job.job.job.name.clone(), job.job.run.url.to_string()),
        };
        let outcome = match (&taken, cancelled) {
            (Ok(_), true) => "cancelled on its forge; reruns once its run has finished",
            (Ok(_), false) => "reruns once its run has finished",
            (Err(_), _) => "not rerun, since the job it took is unknown",
        };
        self.log_job_stop(&job, JobStop::Overload(pressure), outcome);
        if let Err(error) = taken {
            let message = format!(
                "CI job {name}, which protection stopped for {pressure}, is not rerun, since the job its runner took is unknown ({error:#}); rerun it on its forge from {run}"
            );
            self.alert_runner_failed(message).await;
        }
        let event = ended_job(name, run, reason, stopped);
        record_job_ended(&self.store, &job.workspace_id, event).await;
        Ok(true)
    }

    /// Rerun the jobs protection stopped whose runs have finished, reading
    /// their runs as often as a running job's.
    pub async fn rerun_stopped_jobs(&self, daemon: &dyn Daemon) -> Result<()> {
        if !self.jobs.take_reruns_read() {
            return Ok(());
        }
        for job in self.stopped_jobs().await? {
            let rerun = self.rerun(daemon, &job).await;
            if self.rerun_settled(&job, rerun).await {
                (self.forget_job_rerun(&job.run, job.id, &job.repository_id)).await?;
            }
        }
        Ok(())
    }

    /// The claimed jobs whose runner's execution is recorded, which run
    /// whatever their claims' removals wait for.
    pub async fn running_claims(&self, daemon: &dyn Daemon) -> Result<Vec<ClaimedJob>> {
        let mut running = Vec::new();
        for job in self.claimed_jobs().await? {
            if self.job_execution_recorded(daemon, &job).await? {
                running.push(job);
            }
        }
        Ok(running)
    }

    /// Stop the processes of `job`'s workspace under its lifecycle,
    /// recording first that `taken`, when known, is to rerun and having its
    /// forge cancel it, returning the processes stopped and whether the
    /// forge cancelled it: a stop another lifecycle operation refuses
    /// records and cancels nothing, and the record outlives a stop that
    /// fails or a daemon that ends once anything may have been signaled.
    async fn interrupt_job(
        &self,
        daemon: &dyn Daemon,
        job: &ClaimedJob,
        taken: Option<&TakenJob>,
    ) -> Result<(Vec<shoal_protocol::events::StoppedProcess>, bool)> {
        let mut cancelled = false;
        let first = async {
            if let Some(taken) = taken {
                self.record_job_rerun(job, taken).await?;
                cancelled = self.cancel_stopping_job(daemon, job, taken).await;
            }
            Ok(())
        };
        let stopped = (daemon.interrupt_job_processes(&job.workspace_id, first.boxed())).await?;
        Ok((stopped, cancelled))
    }

    /// Have the forge cancel `taken`, which `job`'s runner runs, before the
    /// runner is stopped, so its report of the stop does not fail the job,
    /// returning whether it did. A forge that cannot, or does not answer
    /// within [`CANCEL_LIMIT`], leaves the job to fail.
    async fn cancel_stopping_job(
        &self,
        daemon: &dyn Daemon,
        job: &ClaimedJob,
        taken: &TakenJob,
    ) -> bool {
        let cancel = async {
            let forge = daemon.item_forge(&taken.run).await?;
            let registration = self.runner_registration(&job.repository_id).await?;
            let runner = registration.and_then(|registration| registration.runner);
            (forge.cancel_runner_job(&daemon.user_account(), runner.as_ref(), taken.id)).await
        };
        let cancelled = tokio::time::timeout(CANCEL_LIMIT, cancel)
            .await
            .unwrap_or_else(|_| Err(anyhow::anyhow!("its forge did not answer in time")));
        cancelled.unwrap_or_else(|error| {
            log!(
                "runner: cancel of {} of run {}: {error:#}",
                taken.name,
                taken.run
            );
            false
        })
    }

    /// Whether the execution running `job`'s runner is still recorded, so
    /// the job runs, whatever its claim's removal waits for.
    async fn job_execution_recorded(&self, daemon: &dyn Daemon, job: &ClaimedJob) -> Result<bool> {
        match &job.execution_id {
            Some(execution) => daemon.execution_recorded(execution).await,
            None => Ok(false),
        }
    }

    /// The run and job the runner of `job` runs: the run it reported
    /// taking a job of, which the claim may not name yet, and the claim's
    /// job where the forge hands its runner that job, or else the job the
    /// forge says the runner registered for it alone took. A rerun of a
    /// guess could rerun an unrelated failure, so a job the forge does not
    /// name is an error.
    async fn taken_job(&self, daemon: &dyn Daemon, job: &ClaimedJob) -> Result<TakenJob> {
        let forge = daemon.item_forge(&job.job.run.url).await?;
        // One read of the report names both the run and the job taken of it.
        let reported = reported_run(&self.job_directory(&job.id))?;
        let run_id = reported.map_or(job.job.job.run, |took| took.run);
        let run = match run_id == job.job.job.run {
            true => job.job.run.url.clone(),
            false => forge.run_page(run_id)?,
        };
        let Some(runner) = job.runner_id else {
            return Ok(TakenJob {
                run,
                id: job.job.job.id,
                name: job.job.job.name.clone(),
            });
        };
        let taken = (forge
            .runner_job(&daemon.user_account(), run_id, runner)
            .await)?
            .context("the forge names no job its runner took")?;
        Ok(TakenJob {
            run,
            id: taken.id,
            name: taken.name,
        })
    }

    /// Record that the job `taken`, which `job`'s runner runs, is to rerun.
    async fn record_job_rerun(&self, job: &ClaimedJob, taken: &TakenJob) -> Result<()> {
        let (run, id) = (taken.run.clone(), taken.id);
        let (name, repository) = (taken.name.clone(), job.repository_id.clone());
        self.store
            .write(move |tx| {
                tx.execute(
                    "INSERT OR REPLACE INTO ci_job_reruns(run_url,job_id,job_name,repository_id,stopped_at)
                     VALUES (?1,?2,?3,?4,?5)",
                    params![run, id, name, repository, UnixTime::now()],
                )?;
                Ok(())
            })
            .await
    }

    /// The jobs waiting to rerun.
    async fn stopped_jobs(&self) -> Result<Vec<StoppedJob>> {
        self.store
            .read(|db| {
                Ok(db
                    .prepare(
                        "SELECT run_url,job_id,job_name,repository_id,stopped_at FROM ci_job_reruns",
                    )?
                    .query_map([], |row| {
                        Ok(StoppedJob {
                            run: row.get(0)?,
                            id: row.get(1)?,
                            name: row.get(2)?,
                            repository_id: row.get(3)?,
                            stopped_at: row.get(4)?,
                        })
                    })?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
    }

    /// Have the forge rerun `job` as the user.
    async fn rerun(&self, daemon: &dyn Daemon, job: &StoppedJob) -> Result<JobRerun> {
        let forge = daemon.item_forge(&job.run).await?;
        (forge.rerun_job(&daemon.user_account(), &job.run, job.id)).await
    }

    /// Whether `job` is done waiting, given what its `rerun` found: rerun,
    /// needing none, or past [`PATIENCE`], which tells the user.
    async fn rerun_settled(&self, job: &StoppedJob, rerun: Result<JobRerun>) -> bool {
        let (name, run) = (&job.name, &job.run);
        let expired = UnixTime::now().duration_since(job.stopped_at) >= PATIENCE;
        let failure = match rerun {
            Ok(JobRerun::Rerun) => {
                log!("runner: reran {name} of run {run}, which protection stopped");
                return true;
            }
            Ok(JobRerun::Settled) => return true,
            Ok(JobRerun::Unfinished) if !expired => return false,
            Ok(JobRerun::Unfinished) => "its run has not finished".to_owned(),
            Err(error) if !expired => {
                log!("runner: rerun of {name} of run {run}: {error:#}");
                return false;
            }
            Err(error) => format!("{error:#}"),
        };
        let message = format!(
            "CI job {name}, which protection stopped, was not rerun: {failure}; rerun {run} on its forge"
        );
        self.alert_runner_failed(message).await;
        true
    }

    /// Forget the rerun of the job `id` of the run `run` in `repository`.
    async fn forget_job_rerun(
        &self,
        run: &ItemRef,
        id: JobId,
        repository: &RepositoryId,
    ) -> Result<()> {
        let (run, repository) = (run.clone(), repository.clone());
        self.store
            .write(move |tx| {
                tx.execute(
                    "DELETE FROM ci_job_reruns WHERE run_url=?1 AND job_id=?2 AND repository_id=?3",
                    params![run, id, repository],
                )?;
                Ok(())
            })
            .await
    }

    /// Tell the user `message` about a job protection stopped.
    async fn alert_runner_failed(&self, message: String) {
        let alert = NewAlert::shoal(AlertKind::RunnerFailed, About::default(), message);
        self.notifications.notify_user(alert).await;
    }
}
