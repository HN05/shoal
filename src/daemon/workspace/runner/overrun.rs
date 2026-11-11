//! A job whose runner keeps running after the forge finished the job's run,
//! as when a process a step left behind holds the runner's output open.
//! Once the runner has outlived the run by [`GRACE`], Shoal ends the job
//! itself: it stops every process of the job's workspace, leftovers whose
//! parent is gone included, then the runner, and records what it stopped in
//! the workspace's log. The job then ends as any other does.
use std::time::Duration;

use anyhow::Result;

use super::ends::ClaimedJob;
use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::{
        events::{JobEndedEvent, StoppedProcess},
        space_processes::Selection,
        workspace::{Manager, StopRequest, executions::Save},
    },
    forge::{ForgeRepo, ci::runner::took_job},
};

/// How long a runner may run after the forge finished its job's run, as it
/// reports the job's end.
pub(super) const GRACE: Duration = Duration::from_secs(60);

/// The most stopped processes a job's end lists, so its event fits in a
/// protocol frame; it counts the rest.
const LISTED: usize = 20;

impl Manager {
    /// End `job`, whose runner still runs, once it has outlived the job's
    /// run on the forge by [`GRACE`], returning whether it did.
    pub(super) async fn end_overrun_job(&self, job: &ClaimedJob) -> Result<bool> {
        let Some(run) = self.outlived_run(job).await? else {
            return Ok(false);
        };
        let workspace = self.workspace_by_id(&job.workspace_id).await?;
        let stopped = self
            .stop_workspace_untracked(&workspace, Selection::Processes, None)
            .await?;
        self.stop_job_runner(job).await?;
        let ended = ended_job(job.job.job.name.clone(), run, stopped);
        self.record_job_ended(&job.workspace_id, ended).await;
        Ok(true)
    }

    /// The web URL of the run of `job` once its runner has outlived it by
    /// [`GRACE`], reading the run on the forge when due. A runner registered
    /// for one job that has taken none may still take one of another run,
    /// so it outlives nothing.
    async fn outlived_run(&self, job: &ClaimedJob) -> Result<Option<String>> {
        if job.runner_id.is_some() && !took_job(&self.job_directory(&job.id)) {
            return Ok(None);
        }
        let forge = ForgeRepo::resolve_run_url(&job.job.run.url, &self.forge_hosts()).await?;
        let url = self.reported_run_url(&forge, job);
        if self.runners.take_run_read(&job.id)
            && !(forge.view_run(&self.user_account(), &url).await?.state).unfinished()
        {
            self.runners.run_finished(&job.id);
        }
        let outlived = (self.runners.finished_for(&job.id)).is_some_and(|since| since >= GRACE);
        Ok(outlived.then_some(url))
    }

    /// Stop the runner of `job`, saving nothing: its run is over.
    async fn stop_job_runner(&self, job: &ClaimedJob) -> Result<()> {
        let request = StopRequest {
            reason: "its CI job's run finished on the forge".into(),
            save: Save::Nothing,
        };
        self.stop_workspace(&SpaceSelector::id(&job.workspace_id), request)
            .await
    }
}

/// The event of the job `name` of `run` that Shoal ended after stopping
/// `stopped`.
fn ended_job(name: String, run: String, mut stopped: Vec<StoppedProcess>) -> JobEndedEvent {
    let omitted = stopped.len().saturating_sub(LISTED);
    stopped.truncate(LISTED);
    JobEndedEvent {
        name,
        run,
        stopped,
        omitted,
    }
}

#[cfg(test)]
mod tests;
