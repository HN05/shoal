//! The end of a claimed job: once the execution running the forge's runner
//! ends, however it ended, the runner registered for the job alone is
//! deleted and the job's workspace is removed whatever it holds,
//! with its permit, and the runner's directory of a job no longer claimed,
//! however its workspace went, is deleted. A job ends only on the forge's
//! word: stopping it, removing its workspace while it runs, or a signal
//! ending its wrapper's session cancels its run there, and the forge's
//! runner then stops it, or Shoal or the wrapper does once the runner
//! outlives the run or the signal. Shoal's log names what stopped each
//! runner.
use anyhow::Result;
use shoal_daemon_base::alerts::{About, NewAlert};
use shoal_ids::{execution_ids::ExecutionId, queue_entry_ids::QueueEntryId};
use shoal_protocol::alerts::AlertKind;
use shoal_store::exists;

use super::ClaimedJob;
use crate::{Runner, daemon::Daemon, log};

impl Runner {
    /// Remove the workspace of each claimed job whose execution ended, or
    /// that never started one and whose claim no longer runs, as after a
    /// daemon restart. A removal that fails is tried again on the next pass.
    pub async fn finish_ended_jobs(&self, daemon: &dyn Daemon) -> Result<()> {
        let jobs = self.claimed_jobs().await?;
        (self.jobs).keep_runs(&jobs.iter().map(|job| job.id.clone()).collect());
        for job in jobs {
            let running = match &job.execution_id {
                Some(execution) => {
                    (daemon.job_execution_runs(&job.workspace_id, execution)).await?
                }
                None if self.jobs.claiming(&job.id) => true,
                None => self.adopt_job_execution(daemon, &job).await?,
            };
            if running && !self.ended_overrun_job(daemon, &job).await {
                continue;
            }
            self.finish_job(daemon, &job).await?;
        }
        self.remove_unclaimed_job_directories().await
    }

    /// Have the runner end the job the ended execution `id` ran now, instead
    /// of on its next poll, so its workspace goes as soon as it can.
    pub async fn wake_runner_for_ended_job(&self, id: &ExecutionId) {
        let id = id.to_owned();
        let ran_job = self
            .store
            .run(move |db| exists(db, "SELECT 1 FROM ci_jobs WHERE execution_id=?1", [id]))
            .await;
        // A failed lookup costs only a pass that finds nothing to do.
        if ran_job.unwrap_or(true) {
            self.jobs.wake();
        }
    }

    /// Finish the ended `job`: keep what it used, delete the runner
    /// registered for it alone and remove its workspace, telling the user
    /// of a removal that fails, which the next pass tries again.
    async fn finish_job(&self, daemon: &dyn Daemon, job: &ClaimedJob) -> Result<()> {
        self.record_job_usage(job).await?;
        if let Err(error) = self.delete_job_runner(daemon, job).await {
            log!("runner: {error:#}");
        }
        if let Err(error) = daemon.remove_job_workspace(&job.workspace_id).await {
            let message = format!(
                "The workspace of the ended CI job {} could not be removed: {error:#}",
                job.job.job.name
            );
            let alert = NewAlert::shoal(AlertKind::RunnerFailed, About::default(), message);
            self.notifications.notify_user(alert).await;
        }
        Ok(())
    }

    /// Whether Shoal ended `job`, whose runner outlived its run; a job it
    /// could not end, or whose run could not be read, is tried again on a
    /// later pass.
    async fn ended_overrun_job(&self, daemon: &dyn Daemon, job: &ClaimedJob) -> bool {
        (self.end_overrun_job(daemon, job).await).unwrap_or_else(|error| {
            log!("runner: {}: {error:#}", job.job.job.name);
            false
        })
    }

    /// Record the execution running in the workspace of `job`, whose claim
    /// a restart cut short after its command started but before its
    /// execution was recorded, returning whether there is one.
    async fn adopt_job_execution(&self, daemon: &dyn Daemon, job: &ClaimedJob) -> Result<bool> {
        let Some(execution) = daemon.workspace_execution(&job.workspace_id).await? else {
            return Ok(false);
        };
        self.record_job_execution(&job.id, execution).await?;
        Ok(true)
    }

    /// Delete the runner's directory of each job no longer claimed, however
    /// its workspace went; one that cannot be deleted is tried again on the
    /// next pass.
    async fn remove_unclaimed_job_directories(&self) -> Result<()> {
        let parent = self.paths.state.join("runner");
        let entries = match std::fs::read_dir(&parent) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let entry = entry?;
            let id = QueueEntryId::from(entry.file_name().to_string_lossy().into_owned());
            // Checked for each directory, under the lock a claim writes its
            // files with, so a claim recorded or begun meanwhile keeps them.
            let _files = self.jobs.job_files().await;
            if self.jobs.claiming(&id) || self.job_claimed(&id).await? {
                continue;
            }
            if let Err(error) = std::fs::remove_dir_all(entry.path()) {
                log!("runner: {}: {error:#}", entry.path().display());
            }
        }
        Ok(())
    }

    /// Whether the runner records a claim of the job `id`.
    async fn job_claimed(&self, id: &QueueEntryId) -> Result<bool> {
        let id = id.to_owned();
        self.store
            .run(move |db| exists(db, "SELECT 1 FROM ci_jobs WHERE id=?1", [id]))
            .await
    }
}
