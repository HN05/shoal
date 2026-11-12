//! GitHub's CI checks on a commit, the actions on their jobs and their logs.
use anyhow::Result;

use super::{GitHub, actions, activity};
use crate::{
    ForgeRepo,
    account::Account,
    backends::{Checks, github_rest},
    ci::{CiAction, CiCheck, Job, Location, Operation, checks::plain},
};

impl Checks for GitHub {
    /// Only each check's latest attempt, not those reruns replaced, and
    /// each commit status, which reports no Actions job.
    async fn ci_checks(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        head: &str,
    ) -> Result<Vec<CiCheck>> {
        let (runs, statuses) = activity::commit_checks(repo, account, head).await?;
        let mut checks = actions::checks(&runs)?;
        checks.extend(github_rest::commit_statuses(&statuses, self)?);
        Ok(checks)
    }

    fn locate_job(&self, repo: &ForgeRepo, target: &str) -> Option<Location> {
        actions::locate(repo, target)
    }

    fn ci_operation(
        &self,
        jobs: &[Job],
        selected: &[Job],
        job: &Job,
        action: CiAction,
    ) -> Result<(Operation, Vec<String>)> {
        actions::operation(jobs, selected, job, action)
    }

    /// Each log line starts with its time.
    fn plain_log(&self, log: &str) -> String {
        plain::readable(&plain::without_timestamps(log))
    }

    fn job_log_endpoint(&self, location: Location) -> Result<String> {
        actions::log_endpoint(location)
    }
}
