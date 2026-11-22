//! The CI jobs this machine runs, which the agent queue admits.
use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_agents::daemon::Runner;
use shoal_protocol::time::UnixTime;

use crate::daemon::manager::Manager;
use crate::model::QueuedJob;
use shoal_agents::capacity::Activity;
use shoal_daemon_base::jobs::JobCost;
use shoal_daemon_base::jobs::JobStop;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::queue_entry_ids::QueueEntryId;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;
use shoal_schedule::stopping::RunningJob;
use std::collections::{HashMap, HashSet};

impl Runner for Manager {
    fn job_executions(&self) -> BoxFuture<'_, Result<HashMap<SpaceId, ExecutionId>>> {
        Box::pin(async {
            let jobs = self.runner.claimed_jobs().await?;
            Ok((jobs.into_iter())
                .filter_map(|job| Some((job.workspace_id, job.execution_id?)))
                .collect())
        })
    }

    fn running_jobs(&self) -> BoxFuture<'_, Result<Vec<RunningJob>>> {
        Box::pin(async {
            let jobs = self.runner.running_claims(self).await?;
            let now = UnixTime::now();
            Ok((jobs.into_iter())
                .map(|job| RunningJob {
                    space: job.workspace_id,
                    started: job.launched_at.unwrap_or(now),
                })
                .collect())
        })
    }

    fn runs_job<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, bool> {
        Box::pin(self.runner.runs_job(id))
    }

    fn wake_runner_for_ended_job<'a>(&'a self, id: &'a ExecutionId) -> BoxFuture<'a, ()> {
        Box::pin(self.runner.wake_runner_for_ended_job(id))
    }

    fn job_workspaces(&self) -> BoxFuture<'_, HashSet<SpaceId>> {
        Box::pin(self.runner.job_workspaces())
    }

    fn cancel_job_before_stopping<'a>(
        &'a self,
        id: &'a SpaceId,
        why: JobStop,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.runner.cancel_job_before_stopping(self, id, why).await;
        })
    }

    fn cancel_job_in<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        why: JobStop,
    ) -> BoxFuture<'a, Result<Option<String>>> {
        Box::pin(self.runner.cancel_job_in(self, selector, why))
    }

    fn stop_job_for_overload<'a>(
        &'a self,
        id: &'a SpaceId,
        pressure: &'static str,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(self.runner.stop_job_for_overload(self, id, pressure))
    }

    fn wake_jobs(&self) {
        self.runner.jobs().wake();
    }

    fn job_permits_busy(&self, id: &RepositoryId) -> bool {
        self.runner.jobs().permits_busy(id)
    }

    fn job_cost<'a>(
        &'a self,
        id: &'a RepositoryId,
        job: &'a QueuedJob,
    ) -> BoxFuture<'a, Result<JobCost>> {
        Box::pin(self.runner.job_cost(id, job))
    }

    fn pass_over_job<'a>(
        &'a self,
        id: &'a QueueEntryId,
        repository: &'a RepositoryId,
        job: &'a QueuedJob,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(self.runner.pass_over_job(id, repository, job))
    }

    fn job_activities(&self) -> BoxFuture<'_, Result<Vec<Activity>>> {
        Box::pin(self.job_activities())
    }
}
