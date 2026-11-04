//! The runners a forge registers for one job each, which take a job their
//! labels match rather than the one claimed: kept with the claim, deleted on
//! the forge once the job ends, and deleted before then when no job is left
//! for the runner, which then exits so its workspace goes.
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::params;

use super::{ends::ClaimedJob, runner_labels};
use crate::{
    daemon::workspace::Manager,
    forge::{ForgeRepo, ci::runner::took_job},
};

/// How long a runner may wait for the forge to give it a job before the
/// daemon asks whether one is left for it.
const IDLE_AFTER: Duration = Duration::from_secs(60);

impl Manager {
    /// Keep `runner`, registered for the claimed job `id` alone, with its
    /// claim.
    pub(super) async fn record_job_runner(&self, id: &str, runner: u64) -> Result<()> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE ci_jobs SET runner_id=?2 WHERE id=?1",
                    params![id, i64::try_from(runner)?],
                )?;
                Ok(())
            })
            .await
    }

    /// Delete the runner registered for `job` alone, if any, on its forge.
    pub(super) async fn delete_job_runner(&self, job: &ClaimedJob) -> Result<()> {
        let Some(runner) = job.runner_id else {
            return Ok(());
        };
        let forge = ForgeRepo::resolve_run_url(&job.job.run.url, &self.forge_hosts()).await?;
        forge.delete_runner(&self.user_account(), runner).await
    }

    /// Delete each runner registered for one job that has waited for the
    /// forge to give it one, took none and has no job left to take, so it
    /// exits and its workspace goes. One that cannot be checked is checked
    /// again on the next pass.
    pub(crate) async fn release_idle_runners(&self) -> Result<()> {
        let now = crate::time::unix_seconds() as i64;
        for job in self.claimed_jobs().await? {
            let (Some(runner), Some(launched)) = (job.runner_id, job.launched_at) else {
                continue;
            };
            let waited = Duration::from_secs(now.saturating_sub(launched).max(0) as u64);
            if waited < IDLE_AFTER || took_job(&self.job_directory(&job.id)) {
                continue;
            }
            if let Err(error) = self.release_if_idle(&job, runner).await {
                crate::daemon::log!("runner: {}: {error:#}", job.job.job.name);
            }
        }
        Ok(())
    }

    /// Delete `runner`, registered for `job` alone, when it runs nothing and
    /// the forge lists no job waiting for its labels.
    async fn release_if_idle(&self, job: &ClaimedJob, runner: u64) -> Result<()> {
        let forge = ForgeRepo::resolve_run_url(&job.job.run.url, &self.forge_hosts()).await?;
        let account = self.user_account();
        // A runner already gone exits by itself.
        if forge.runner_busy(&account, runner).await? != Some(false) {
            return Ok(());
        }
        let settings = self
            .repository_settings(&job.repository_id)
            .await
            .context("read the repository's runner settings")?;
        let labels = runner_labels(&settings.runner.labels);
        if forge.waiting_jobs(&account, &labels).await?.is_empty() {
            forge.delete_runner(&account, runner).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
