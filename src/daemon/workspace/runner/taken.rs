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
        ci::runner::{answer_took_run, took_run},
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
            let run = match took_run(&directory) {
                Ok(Some(run)) => run,
                Ok(None) => continue,
                Err(error) => {
                    crate::daemon::log!("runner: {}: {error:#}", job.job.job.name);
                    continue;
                }
            };
            let answer = self
                .follow_taken_job(&job, runner, run)
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

    /// Record that `runner`, registered for `job` alone, took a job of
    /// `run`, move the job's workspace to the commit of `run`, and then name
    /// the job it took.
    async fn follow_taken_job(&self, job: &ClaimedJob, runner: u64, run: u64) -> Result<()> {
        let forge = ForgeRepo::resolve_run_url(&job.job.run.url, &self.forge_hosts()).await?;
        let account = self.user_account();
        let taken_run = if run == job.job.job.run {
            job.job.run.clone()
        } else {
            forge.job_run(&account, run).await?
        };
        let linked = self
            .run_workspace(&forge, &job.repository_id, &taken_run)
            .await?;
        let mut record = QueuedJob {
            job: job.job.job.clone(),
            run: taken_run,
            workspace_id: linked,
        };
        // Recorded first, so stopping the job cancels the run it belongs
        // to whatever fails next.
        self.record_taken_job(&job.id, &record).await?;
        self.move_job_workspace(
            &job.workspace_id,
            &record.run.commit,
            record.run.reference.as_deref(),
        )
        .await?;
        // Only the run decides the commit; a job the forge does not name
        // keeps the claimed one's name.
        match forge.runner_job(&account, run, runner).await {
            Ok(Some(taken)) => {
                record.job = taken;
                self.record_taken_job(&job.id, &record).await?;
            }
            Ok(None) => {}
            Err(error) => crate::daemon::log!("runner: {}: {error:#}", job.job.job.name),
        }
        Ok(())
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
