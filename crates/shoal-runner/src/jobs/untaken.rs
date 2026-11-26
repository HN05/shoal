//! Claimed jobs whose runner never takes them, which the waiting-jobs poll
//! stops. Such a runner keeps no run for its job's cost: it used nothing a
//! run does.
use std::collections::HashSet;

use anyhow::{Context, Result};
use shoal_forge::ci::runner::{JobTaker, RegisteredRunner, RunnerRequests, took_job};
use shoal_forge_types::ForgeRepo;
use shoal_ids::repository_ids::RepositoryId;

use super::ClaimedJob;
use crate::{Runner, daemon::Daemon, log};
use shoal_daemon_base::jobs::JobStop;

impl Runner {
    /// Stop the runner of each running job of `repository_id` that `forge`
    /// no longer lists among the `listed` handles and that a runner other
    /// than `ours` took, or none. A job whose taker cannot be read, or
    /// whose runner cannot be stopped, is checked again on the next poll.
    pub(super) async fn end_untaken_jobs(
        &self,
        daemon: &dyn Daemon,
        repository_id: &RepositoryId,
        forge: &ForgeRepo,
        ours: &RegisteredRunner,
        listed: &HashSet<String>,
    ) -> Result<()> {
        for job in self.claimed_jobs().await? {
            if job.repository_id != *repository_id || !self.awaits_taker(&job, listed) {
                continue;
            }
            let ended = async {
                if self.taken_elsewhere(forge, &job, ours).await? {
                    self.end_untaken_job(daemon, &job).await?;
                }
                anyhow::Ok(())
            };
            if let Err(error) = ended.await {
                log!("runner: {}: {error:#}", job.job.job.name);
            }
        }
        Ok(())
    }

    /// Whether `job` runs a runner, which may have taken it, that the forge
    /// has not said took it, though it no longer lists it among `listed`.
    fn awaits_taker(&self, job: &ClaimedJob, listed: &HashSet<String>) -> bool {
        job.execution_id.is_some()
            && !listed.contains(&job.job.job.handle)
            && !self.jobs.taken(&job.id)
    }

    /// Whether the forge says a runner other than `ours` took `job`, or
    /// none will, remembering one `ours` took.
    async fn taken_elsewhere(
        &self,
        forge: &ForgeRepo,
        job: &ClaimedJob,
        ours: &RegisteredRunner,
    ) -> Result<bool> {
        let account = self.forges.user_account();
        Ok(match forge.job_taker(&account, &job.job.job).await? {
            JobTaker::Pending => false,
            JobTaker::Runner(uuid) if uuid == ours.uuid => {
                self.jobs.took(&job.id);
                false
            }
            JobTaker::Runner(_) | JobTaker::Nobody => true,
        })
    }

    /// Stop the runner of `job`, which never took it, keeping no run of it
    /// for its job's cost.
    async fn end_untaken_job(&self, daemon: &dyn Daemon, job: &ClaimedJob) -> Result<()> {
        self.discard_job_usage(&job.id).await?;
        let reason = "another runner took its CI job, or none will".to_owned();
        daemon.stop_job_runner(&job.workspace_id, reason).await?;
        self.log_job_stop(job, JobStop::Untaken, "it had taken no job");
        Ok(())
    }

    /// Keep what the ended `job` used as a run of its job, unless its runner
    /// never took it, as one that found its job taken already and exited.
    pub(super) async fn keep_job_usage(&self, job: &ClaimedJob) -> Result<()> {
        if !self.job_usage_unkept(&job.id).await? {
            return Ok(());
        }
        if self.runner_took_job(job).await {
            self.record_job_usage(job).await
        } else {
            self.discard_job_usage(&job.id).await
        }
    }

    /// Whether the runner of the ended `job` took a job: one registered for
    /// one job by its hook's report, one that claimed its job by handle by
    /// the forge's word that this machine's runner took it. One the forge
    /// cannot be asked about counts as having taken none, as does one that
    /// ended before the forge names it, having run nothing of the job.
    async fn runner_took_job(&self, job: &ClaimedJob) -> bool {
        if job.runner_id.is_some() {
            return took_job(&self.job_directory(&job.id));
        }
        if self.jobs.taken(&job.id) {
            return true;
        }
        (self.taken_by_ours(job).await).unwrap_or_else(|error| {
            log!("runner: {}: {error:#}", job.job.job.name);
            false
        })
    }

    /// Whether the forge names the repository's runner as the one that took
    /// `job`.
    async fn taken_by_ours(&self, job: &ClaimedJob) -> Result<bool> {
        let registration = (self.runner_registration(&job.repository_id).await?)
            .context("the repository's runner is no longer registered")?;
        let ours = (registration.runner).context("the repository has no runner of its own")?;
        let forge = self.forges.forge(&registration.forge_repository).await?;
        let account = self.forges.user_account();
        let taker = forge.job_taker(&account, &job.job.job).await?;
        Ok(matches!(taker, JobTaker::Runner(uuid) if uuid == ours.uuid))
    }
}
