//! Claimed jobs whose runner never takes them. A runner of the repository's
//! own, as Forgejo keeps, claims its job by handle and takes that job or
//! none: once the forge no longer lists the job as waiting and says another
//! runner took it, or that none did, the runner holds its job slot and the
//! job's expected memory for nothing. The waiting-jobs poll that sees it
//! stops the runner, and the job then ends as any other does, keeping no
//! run for its job's cost. A runner registered for one job takes any its
//! labels match, and goes once none is left for it.
use std::collections::HashSet;

use anyhow::Result;
use shoal_forge::ci::runner::{JobTaker, RegisteredRunner, RunnerRequests};
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
}
