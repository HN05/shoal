//! Cancelling a claimed job's run on the forge before its runner stops, since
//! a job ends only on the forge's word.
use std::collections::HashSet;

use anyhow::{Context as _, Result};
use shoal_daemon_base::{
    alerts::{About, NewAlert},
    callers::Caller,
};
use shoal_forge::ci::run_action::RunActionRequests;
use shoal_forge_types::{ci::CiAction, items::reference::ItemRef};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{alerts::AlertKind, protocol::SessionSignal, space_names::SpaceSelector};

use super::ClaimedJob;
use crate::{Runner, daemon::Daemon, log};
use shoal_daemon_base::jobs::JobStop;

impl Runner {
    /// Cancel on the forge the run of the job running in the workspace
    /// `selector` names, as `cause` stops it, returning the run's URL, or
    /// `None` when the workspace runs no job.
    pub async fn cancel_job_in(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        cause: JobStop,
    ) -> Result<Option<String>> {
        let space = daemon.select_space_id(selector).await?;
        let Some(job) = self.workspace_claim(&space).await? else {
            return Ok(None);
        };
        Ok(Some(self.cancel_for_stop(&job, cause).await?.to_string()))
    }

    /// Cancel on the forge the run of a job still running in the workspace
    /// `workspace_id` before `cause` has the daemon stop its runner,
    /// returning whether the workspace holds a job; a forge that refuses is
    /// told to the user, and the run times out there.
    pub async fn cancel_job_before_stopping(
        &self,
        daemon: &dyn Daemon,
        workspace_id: &SpaceId,
        cause: JobStop,
    ) -> bool {
        let Ok(Some(job)) = self.workspace_claim(workspace_id).await else {
            return false;
        };
        // The runner may run before its claim records its execution, so
        // any execution of the workspace counts.
        let running = match &job.execution_id {
            Some(execution) => daemon.execution_recorded(execution).await,
            None => daemon.workspace_has_executions(workspace_id).await,
        };
        if !running.unwrap_or(true) {
            return true;
        }
        if let Err(error) = self.cancel_for_stop(&job, cause).await {
            self.notify_cancel_failed(&job, &error).await;
        }
        true
    }

    /// Cancel on the forge the job the execution of `caller` runs, if it
    /// runs one, to rerun, before `signal`, which ends its session, has its
    /// wrapper stop the runner. Another execution in the job's workspace,
    /// such as a step's command, cancels nothing.
    pub async fn cancel_job_for_signal(
        &self,
        caller: Option<&Caller>,
        signal: SessionSignal,
    ) -> Result<()> {
        let caller = caller
            .filter(|caller| caller.execution_id().is_some())
            .context("only an execution's wrapper reports the signal that ends its session")?;
        let Some(job) = self.workspace_claim(&caller.space_id).await? else {
            return Ok(());
        };
        // The runner may run before its claim records its execution.
        let runner = (job.execution_id.as_ref()).is_none_or(|id| caller.execution_id() == Some(id));
        if !runner {
            return Ok(());
        }
        let cancelled = (self.cancel_job_to_rerun(&job, JobStop::Signal(signal))).await;
        if let Err(error) = cancelled {
            self.notify_cancel_failed(&job, &error).await;
            return Err(error);
        }
        Ok(())
    }

    /// Whether the space `space_id` runs a claimed job, whose runner's
    /// wrapper reports the signal that ends its session.
    pub async fn runs_job(&self, space_id: &SpaceId) -> bool {
        self.workspace_claim(space_id)
            .await
            .is_ok_and(|job| job.is_some())
    }

    /// The workspaces of the claimed jobs, none when they cannot be read.
    pub async fn job_workspaces(&self) -> HashSet<SpaceId> {
        match self.claimed_jobs().await {
            Ok(jobs) => jobs.into_iter().map(|job| job.workspace_id).collect(),
            Err(error) => {
                log!("runner: claimed jobs unavailable: {error:#}");
                HashSet::new()
            }
        }
    }

    /// Tell the user that stopping `job` could not cancel its run.
    async fn notify_cancel_failed(&self, job: &ClaimedJob, error: &anyhow::Error) {
        let message = format!(
            "Stopping CI job {} could not cancel {} on the forge, where it times out: {error:#}",
            job.job.job.name, job.job.run.url
        );
        let alert = NewAlert::shoal(AlertKind::RunnerFailed, About::default(), message);
        self.notifications.notify_user(alert).await;
    }

    pub(super) async fn workspace_claim(
        &self,
        workspace_id: &SpaceId,
    ) -> Result<Option<ClaimedJob>> {
        Ok((self.claimed_jobs().await?)
            .into_iter()
            .find(|job| job.workspace_id == *workspace_id))
    }

    /// Cancel `job`'s run as `cause` stops its runner, cutting it short,
    /// recording in Shoal's log what stopped it and whether the run was
    /// cancelled, returning the cancelled run's URL.
    async fn cancel_for_stop(&self, job: &ClaimedJob, cause: JobStop) -> Result<ItemRef> {
        self.mark_job_cut_short(&job.id).await?;
        let cancelled = self.cancel_job_run(job).await;
        let outcome = match &cancelled {
            Ok(_) => "cancelled its run".to_owned(),
            Err(error) => format!("could not cancel its run: {error:#}"),
        };
        self.log_job_stop(job, cause, &outcome);
        cancelled
    }

    /// Cancel `job`'s run on the forge its URL names, or the run its
    /// runner reported taking a job of, also once its runner is gone,
    /// returning the cancelled run's URL.
    pub async fn cancel_job_run(&self, job: &ClaimedJob) -> Result<ItemRef> {
        let forge = self.forges.item_forge(&job.job.run.url).await?;
        let account = self.forges.user_account();
        let url = self.reported_run_url(&forge, job);
        let run = forge.find_run(&account, &url, CiAction::Cancel).await?;
        forge
            .act_on_run(&account, &run, CiAction::Cancel, false)
            .await?;
        Ok(url)
    }
}
