//! The job a runner registered for one job took, which the forge chose: its
//! job-started hook reports the job's run before the first step. The claim
//! then records that job, and a job of another run than the claimed one
//! waits until its workspace is at that run's commit; a workspace that
//! cannot move fails the job instead of running it on the wrong commit.
use anyhow::{Context, Result};
use rusqlite::params;

use super::ends::ClaimedJob;
use crate::{
    daemon::workspace::Manager,
    forge::{
        ForgeRepo,
        ci::runner::{JobRun, TookRun, WaitingJob, answer_took_run, reported_run, took_run},
    },
    model::QueuedJob,
};

impl Manager {
    /// Follow each running job whose runner reported the run of the job it
    /// took and has no answer yet. One whose report cannot be read or
    /// answered is tried again on the next pass.
    pub(crate) async fn follow_taken_jobs(&self) -> Result<()> {
        for job in self.claimed_jobs().await? {
            let Some(runner) = job.runner_id else {
                continue;
            };
            let directory = self.job_directory(&job.id);
            let took = match took_run(&directory) {
                Ok(Some(took)) => took,
                Ok(None) => continue,
                Err(error) => {
                    crate::daemon::log!("runner: {}: {error:#}", job.job.job.name);
                    continue;
                }
            };
            let answer = self
                .follow_taken_job(&job, runner, took)
                .await
                .map_err(|error| format!("Shoal could not move the job's workspace: {error:#}"));
            if let Err(reason) = &answer {
                crate::daemon::log!("runner: {}: {reason}", job.job.job.name);
            }
            if let Err(error) = answer_took_run(&directory, answer) {
                crate::daemon::log!("runner: {}: {error:#}", job.job.job.name);
            }
        }
        Ok(())
    }

    /// Record that `runner`, registered for `job` alone, took a job of the
    /// run `took` names, move the job's workspace to that run's commit when
    /// its hook holds the job back for it, and then name the job it took.
    async fn follow_taken_job(&self, job: &ClaimedJob, runner: u64, took: TookRun) -> Result<()> {
        let forge = ForgeRepo::resolve_run_url(&job.job.run.url, &self.forge_hosts()).await?;
        let account = self.user_account();
        let taken_run = self.taken_run(&forge, job, took.run).await?;
        let linked = self
            .run_workspace(&forge, &job.repository_id, &taken_run)
            .await?;
        let mut record = QueuedJob {
            job: WaitingJob {
                run: took.run,
                ..job.job.job.clone()
            },
            run: taken_run,
            workspace_id: linked,
        };
        // Recorded first, so stopping the job cancels the run it belongs
        // to whatever fails next.
        self.record_taken_job(&job.id, &record).await?;
        // A job its hook let go on runs its steps already, in a checkout
        // they may change.
        if took.awaits {
            self.move_job_workspace(
                &job.workspace_id,
                &record.run.commit,
                record.run.reference.as_deref(),
            )
            .await?;
        }
        // Only the run decides the commit; a job the forge does not name
        // keeps the claimed one's name.
        match forge.runner_job(&account, took.run, runner).await {
            Ok(Some(taken)) => {
                record.job = taken;
                self.record_taken_job(&job.id, &record).await?;
            }
            Ok(None) => {}
            Err(error) => crate::daemon::log!("runner: {}: {error:#}", job.job.job.name),
        }
        Ok(())
    }

    /// The run with API ID `run` that the runner of `job` took a job of. One
    /// that cannot be read is still recorded by its URL, so stopping the job
    /// cancels it.
    async fn taken_run(&self, forge: &ForgeRepo, job: &ClaimedJob, run: u64) -> Result<JobRun> {
        if run == job.job.job.run {
            return Ok(job.job.run.clone());
        }
        let error = match forge.job_run(&self.user_account(), run).await {
            Ok(taken) => return Ok(taken),
            Err(error) => error,
        };
        // A forge whose runner takes another run's job serves a run's page
        // under its API ID.
        let (_, url) = forge.run(&run.to_string())?;
        let record = QueuedJob {
            run: JobRun {
                url,
                ..job.job.run.clone()
            },
            ..job.job.clone()
        };
        self.record_taken_job(&job.id, &record).await?;
        Err(error)
    }

    /// The web URL of the run `job`'s runner took a job of: the one it
    /// reported, which a claim read before the daemon followed the report
    /// does not name yet, else the claim's.
    pub(super) fn reported_run_url(&self, forge: &ForgeRepo, job: &ClaimedJob) -> String {
        match reported_run(&self.job_directory(&job.id)) {
            Ok(Some(took)) if took.run != job.job.job.run => forge
                .run(&took.run.to_string())
                .map(|(_, url)| url)
                .unwrap_or_else(|_| job.job.run.url.clone()),
            _ => job.job.run.url.clone(),
        }
    }

    async fn record_taken_job(&self, id: &str, job: &QueuedJob) -> Result<()> {
        let (id, job) = (
            id.to_owned(),
            serde_json::to_string(job).context("record the job the runner took")?,
        );
        self.store
            .run(move |db| {
                db.execute("UPDATE ci_jobs SET job=?2 WHERE id=?1", params![id, job])?;
                Ok(())
            })
            .await
    }
}

#[cfg(test)]
mod tests;
