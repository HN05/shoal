//! GitLab's CI checks on a commit, the actions on their jobs and their logs.
use anyhow::{Result, bail};
use serde_json::Value;

use super::{GitLab, latest_statuses, status_state};
use crate::{
    ForgeRepo,
    account::Account,
    backends::Checks,
    ci::{CiAction, CiCheck, Job, Location, Operation, check, checks::plain, id},
    numbers::JobId,
};

impl Checks for GitLab {
    async fn ci_checks(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        head: &str,
    ) -> Result<Vec<CiCheck>> {
        let endpoint = repo.repo_endpoint(&format!("repository/commits/{head}/statuses"));
        latest_checks(&repo.pages(account, &endpoint).await?)
    }

    fn locate_job(&self, repo: &ForgeRepo, target: &str) -> Option<Location> {
        let parts = repo.repository_path(target, "/-/jobs/")?;
        let [job] = parts.as_slice() else {
            return None;
        };
        Some(Location::GitLab {
            job: JobId::new(id(job)?),
        })
    }

    fn ci_operation(
        &self,
        _jobs: &[Job],
        _selected: &[Job],
        job: &Job,
        action: CiAction,
    ) -> Result<(Operation, Vec<String>)> {
        let Some(Location::GitLab { job: id }) = job.location else {
            bail!("check {} is not a GitLab job", job.name);
        };
        let endpoint = match action {
            CiAction::Rerun => format!("jobs/{id}/retry"),
            CiAction::Cancel => format!("jobs/{id}/cancel"),
        };
        Ok((Operation::Post(endpoint), vec![job.name.clone()]))
    }

    /// Traces mark each end of a collapsible section.
    fn plain_log(&self, log: &str) -> String {
        plain::readable(&plain::without_sections(log))
    }

    /// A job's trace is its log.
    fn job_log_endpoint(&self, location: Location) -> Result<String> {
        let Location::GitLab { job } = location else {
            bail!("not a GitLab job");
        };
        Ok(format!("jobs/{job}/trace"))
    }
}

fn latest_checks(statuses: &[Value]) -> Result<Vec<CiCheck>> {
    latest_statuses(statuses)?
        .into_iter()
        .map(|(_, status)| {
            let state = status_state(&status["status"])?.into();
            check(status, "name", state, "target_url")
        })
        .collect()
}

#[cfg(test)]
mod tests;
