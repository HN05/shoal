//! The runners a forge registers for one job each, which take a job their
//! labels match rather than the one claimed: kept with the claim, deleted on
//! the forge once the job ends, and deleted before then when no job is left
//! for the runner, which then exits so its workspace goes.
use std::{path::PathBuf, time::Duration};

use anyhow::Result;
use rusqlite::params;
use shoal_forge::ci::runner::{RunnerRequests, took_job};
use shoal_forge_types::numbers::RunnerId;
use shoal_ids::queue_entry_ids::QueueEntryId;

use super::ClaimedJob;
use crate::{Runner, daemon::Daemon, log, time::UnixTime};

/// How long a runner may wait for the forge to give it a job before the
/// daemon asks whether one is left for it.
const IDLE_AFTER: Duration = Duration::from_secs(60);

impl Runner {
    /// The directory a claimed job's runner keeps its files in.
    pub fn job_directory(&self, id: &QueueEntryId) -> PathBuf {
        self.paths.state.join("runner").join(id)
    }

    /// Keep `runner`, registered for the claimed job `id` alone, with its
    /// claim.
    pub async fn record_job_runner(&self, id: &QueueEntryId, runner: RunnerId) -> Result<()> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE ci_jobs SET runner_id=?2 WHERE id=?1",
                    params![id, runner],
                )?;
                Ok(())
            })
            .await
    }

    /// Delete the runner registered for `job` alone, if any, on its forge.
    pub async fn delete_job_runner(&self, daemon: &dyn Daemon, job: &ClaimedJob) -> Result<()> {
        let Some(runner) = job.runner_id else {
            return Ok(());
        };
        let forge = daemon.item_forge(&job.job.run.url).await?;
        forge.delete_runner(&daemon.user_account(), runner).await
    }

    /// Delete each runner registered for one job that has waited for the
    /// forge to give it one, took none and has no job left to take, so it
    /// exits and its workspace goes. One that cannot be checked is checked
    /// again on the next pass.
    pub async fn release_idle_runners(&self, daemon: &dyn Daemon) -> Result<()> {
        let now = UnixTime::now();
        for job in self.claimed_jobs().await? {
            let (Some(runner), Some(launched)) = (job.runner_id, job.launched_at) else {
                continue;
            };
            let waited = now.duration_since(launched);
            if waited < IDLE_AFTER || took_job(&self.job_directory(&job.id)) {
                continue;
            }
            if let Err(error) = release_if_idle(daemon, &job, runner).await {
                log!("runner: {}: {error:#}", job.job.job.name);
            }
        }
        Ok(())
    }
}

/// Delete `runner`, registered for `job` alone, when it runs nothing and the
/// forge lists no job waiting for the labels it was registered with.
async fn release_if_idle(daemon: &dyn Daemon, job: &ClaimedJob, runner: RunnerId) -> Result<()> {
    let forge = daemon.item_forge(&job.job.run.url).await?;
    let account = daemon.user_account();
    // A runner already gone exits by itself.
    let Some(state) = forge.runner_state(&account, runner).await? else {
        return Ok(());
    };
    if !state.busy
        && forge
            .waiting_jobs(&account, &state.labels)
            .await?
            .is_empty()
    {
        forge.delete_runner(&account, runner).await?;
    }
    Ok(())
}
