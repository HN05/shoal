//! Rerunning and cancelling one CI workflow run, by its web URL or the
//! number in it, as the workspace's agent account. A PR's checks are acted
//! on through [`shoal_forge_types::ci::CiAction`] on its head instead.
use anyhow::{Context, Result, ensure};

use crate::{
    RepoRequests,
    backends::Runs,
    capability::CapabilityRequests,
    ci::run_view::RunViewSteps,
    transport::account::Account,
    transport::api::{Request, TransportRequests},
};
use shoal_forge_types::{
    ForgeRepo,
    ci::{
        CiAction,
        run_action::{Run, RunActed},
    },
    items::{link::LinkKind, reference::ItemRef},
    numbers::JobId,
    raw::HttpMethod,
};

/// What became of a job [`RunActionRequests::rerun_job`] was to rerun.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobRerun {
    /// The forge reruns it.
    Rerun,
    /// Its run has yet to finish, so it cannot be rerun yet.
    Unfinished,
    /// It passed, or its run no longer has it, so it needs no rerun.
    Settled,
}

/// The requests that apply an action to a run.
pub(crate) enum RunOperation {
    /// POST to this endpoint under the repository's.
    Post(String),
    /// POST to the endpoint under the repository's that each failed job's ID
    /// gives, one job at a time.
    EachFailedJob(fn(JobId) -> String),
}

/// Finding runs and acting on them.
#[shoal_forge_derive::extension(pub RunActionRequests, pub(crate) RunActionSteps)]
impl ForgeRepo {
    /// The run `run`, once the forge is known to apply `action` to runs.
    pub async fn find_run(
        &self,
        account: &Account,
        run: &ItemRef,
        action: CiAction,
    ) -> Result<Run> {
        self.require_on_server(account, action.capability()).await?;
        self.read_run(account, run).await
    }

    /// The run `run`.
    pub(crate) async fn read_run(&self, account: &Account, run: &ItemRef) -> Result<Run> {
        run.belongs_to(self)?;
        let number = run.run()?;
        let read = Request::get(self.repo_endpoint(&self.kind().run_path(number)));
        let value = self.send(account, &read).await?;
        self.kind().run(self, number, &value)
    }

    /// The branch of this repository `run` is of: the head branch of the PR
    /// it ran for, whatever commit of the PR it ran on, since a forge may
    /// name the PR as its branch, otherwise the branch it ran on. A fork's
    /// PR has none here.
    pub async fn run_branch(&self, account: &Account, run: &Run) -> Result<Option<String>> {
        let Some(number) = run.pull else {
            return Ok(run.branch.clone());
        };
        let url = ItemRef::numbered(self, LinkKind::Pr, number.get())?;
        let pull = (self.pull_request(account, &url).await)
            .with_context(|| format!("could not read {url}, which {} ran for", run.url))?;
        Ok((!pull.fork).then(|| pull.head.to_string()))
    }

    /// Cancel `run` unless it has finished.
    pub async fn cancel_unfinished(&self, account: &Account, run: &ItemRef) -> Result<()> {
        let run = self.find_run(account, run, CiAction::Cancel).await?;
        if run.state.unfinished() {
            self.act_on_run(account, &run, CiAction::Cancel, false)
                .await?;
        }
        Ok(())
    }

    /// Apply `action` to `run`; `failed` reruns only its failed jobs.
    pub async fn act_on_run(
        &self,
        account: &Account,
        run: &Run,
        action: CiAction,
        failed: bool,
    ) -> Result<RunActed> {
        match self.kind().run_operation(run, action, failed)? {
            RunOperation::Post(endpoint) => self.post(account, &endpoint).await?,
            RunOperation::EachFailedJob(endpoint) => {
                self.rerun_failed_jobs(account, run, endpoint).await?
            }
        }
        Ok(RunActed {
            url: run.url.clone(),
            action,
        })
    }

    /// Rerun the job `id` of the run `url` once the run has finished with
    /// it failed or cancelled. A later attempt of the job has an ID of its
    /// own, so a run without `id` needs no rerun of it.
    pub async fn rerun_job(&self, account: &Account, url: &ItemRef, id: JobId) -> Result<JobRerun> {
        let run = self.find_run(account, url, CiAction::Rerun).await?;
        if run.state.unfinished() {
            return Ok(JobRerun::Unfinished);
        }
        let jobs = self.jobs_of(account, &run).await?;
        let Some(job) = jobs.iter().find(|job| job.id == id) else {
            return Ok(JobRerun::Settled);
        };
        if !job.state.rerunnable() {
            return Ok(JobRerun::Settled);
        }
        let endpoint = self.kind().rerun_job_endpoint(job.id)?;
        (self.post(account, &endpoint).await)
            .with_context(|| format!("could not rerun job {}", job.name))?;
        Ok(JobRerun::Rerun)
    }

    /// Rerun each failed job of `run` at the endpoint its ID gives. A rerun
    /// also restarts the jobs that need the rerun one, so the jobs are read
    /// again before each rerun, and only one still failed is rerun.
    async fn rerun_failed_jobs(
        &self,
        account: &Account,
        run: &Run,
        endpoint: fn(JobId) -> String,
    ) -> Result<()> {
        let mut rerun = Vec::new();
        loop {
            let jobs = self.jobs_of(account, run).await?;
            let Some(job) = jobs
                .iter()
                .find(|job| job.state.rerunnable() && !rerun.contains(&job.id))
            else {
                break;
            };
            self.post(account, &endpoint(job.id))
                .await
                .with_context(|| format!("could not rerun job {}", job.name))?;
            rerun.push(job.id);
        }
        ensure!(!rerun.is_empty(), "no job of {} has failed", run.url);
        Ok(())
    }

    /// POST to `endpoint` under the repository's.
    async fn post(&self, account: &Account, endpoint: &str) -> Result<()> {
        let request = Request::new(HttpMethod::Post, self.repo_endpoint(endpoint), None);
        self.send(account, &request).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
