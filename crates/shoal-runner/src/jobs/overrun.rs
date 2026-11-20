//! A job whose runner keeps running after the forge finished the job's run,
//! as when a process a step left behind holds the runner's output open.
//! Once the runner has outlived the run by [`GRACE`], Shoal ends the job
//! itself: it stops every process of the job's workspace, leftovers whose
//! parent is gone included, then the runner, and records what it stopped in
//! the workspace's log. The job then ends as any other does.
use std::time::Duration;

use anyhow::Result;
use shoal_daemon_base::events::record_job_ended;
use shoal_forge::ci::{run_view::RunViewRequests, runner::took_job};
use shoal_protocol::events::{JobEndedEvent, StoppedProcess};

use super::{ClaimedJob, JobStop};
use crate::{Runner, daemon::Daemon};

/// How long a runner may run after the forge finished its job's run, as it
/// reports the job's end.
pub const GRACE: Duration = Duration::from_secs(60);

/// The most stopped processes a job's end lists, so its event fits in a
/// protocol frame; it counts the rest.
const LISTED: usize = 20;

impl Runner {
    /// End `job`, whose runner still runs, once it has outlived the job's
    /// run on the forge by [`GRACE`], returning whether it did.
    pub async fn end_overrun_job(&self, daemon: &dyn Daemon, job: &ClaimedJob) -> Result<bool> {
        let Some(run) = self.outlived_run(daemon, job).await? else {
            return Ok(false);
        };
        let stopped = daemon.stop_job_processes(&job.workspace_id).await?;
        let reason = "its CI job's run finished on the forge".to_owned();
        daemon.stop_job_runner(&job.workspace_id, reason).await?;
        self.log_job_stop(job, JobStop::Overrun, "its run had finished");
        let reason = "its runner outlived its finished run".into();
        let ended = ended_job(job.job.job.name.clone(), run, reason, stopped);
        record_job_ended(&self.store, &job.workspace_id, ended).await;
        Ok(true)
    }

    /// The web URL of the run of `job` once its runner has outlived it by
    /// [`GRACE`], reading the run on the forge when due. A runner registered
    /// for one job that has taken none may still take one of another run,
    /// so it outlives nothing.
    async fn outlived_run(&self, daemon: &dyn Daemon, job: &ClaimedJob) -> Result<Option<String>> {
        if job.runner_id.is_some() && !took_job(&self.job_directory(&job.id)) {
            return Ok(None);
        }
        let forge = daemon.item_forge(&job.job.run.url).await?;
        let url = self.reported_run_url(&forge, job);
        if self.jobs.take_run_read(&job.id)
            && !(forge.view_run(&daemon.user_account(), &url).await?.state).unfinished()
        {
            self.jobs.run_finished(&job.id);
        }
        let outlived = (self.jobs.finished_for(&job.id)).is_some_and(|since| since >= GRACE);
        Ok(outlived.then(|| url.to_string()))
    }
}

/// The event of the job `name` of `run` that Shoal ended for `reason`
/// after stopping `stopped`.
pub(super) fn ended_job(
    name: String,
    run: String,
    reason: String,
    mut stopped: Vec<StoppedProcess>,
) -> JobEndedEvent {
    let omitted = stopped.len().saturating_sub(LISTED);
    stopped.truncate(LISTED);
    JobEndedEvent {
        name,
        run,
        reason,
        stopped,
        omitted,
    }
}

#[cfg(test)]
mod tests;
