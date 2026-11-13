//! Forgejo's CI checks on a commit, the actions on their jobs and their logs.
use anyhow::Result;

use super::{Forgejo, actions};
use crate::{
    ForgeRepo,
    account::Account,
    backends::{Checks, github_rest},
    ci::{
        CiAction, CiCheck, Job, Location, Operation,
        checks::{CheckLog, plain},
    },
    numbers::JobId,
    transport::api::Request,
};

impl Checks for Forgejo {
    async fn ci_checks(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        head: &str,
    ) -> Result<Vec<CiCheck>> {
        let endpoint = repo.repo_endpoint(&format!("commits/{head}/status?limit=50"));
        github_rest::commit_statuses(&repo.send(account, &Request::get(endpoint)).await?, self)
    }

    fn locate_job(&self, repo: &ForgeRepo, target: &str) -> Option<Location> {
        let (run_number, job_index) = actions::job_location(repo, target).ok()?;
        Some(Location::Forgejo {
            run_number,
            job_index,
        })
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

    /// Forgejo links a job by its run's number and its index in the run, not
    /// the API IDs its actions take.
    async fn ci_endpoint(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        head: &str,
        operation: Operation,
    ) -> Result<String> {
        match operation {
            Operation::Post(endpoint) => Ok(endpoint),
            // The run that posted a check on the head holds it, wherever
            // it ran, so cancelling it frees the check; rerunning a run of
            // an older commit would not.
            Operation::ForgejoRun { run_number, action } => {
                let (endpoint, on_head) = match action {
                    CiAction::Rerun => ("rerun", Some(head)),
                    CiAction::Cancel => ("cancel", None),
                };
                let run = actions::run_id_for(repo, account, run_number, on_head).await?;
                Ok(format!("actions/runs/{run}/{endpoint}"))
            }
            Operation::RerunForgejoJob {
                run_number,
                job_index,
            } => {
                let job =
                    actions::failed_job_id(repo, account, head, run_number, job_index).await?;
                Ok(format!("actions/jobs/{job}/rerun"))
            }
        }
    }

    async fn failed_check_log(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        pull: &crate::items::reference::ItemRef,
        name: &str,
    ) -> Result<CheckLog> {
        actions::failed_check_log(repo, account, pull, name).await
    }

    /// Each log line starts with its time.
    fn plain_log(&self, log: &str) -> String {
        plain::readable(&plain::without_timestamps(log))
    }

    fn failed_step(&self, log: &str) -> Option<String> {
        actions::failed_step(log)
    }

    async fn listed_failed_step(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        job: JobId,
    ) -> Option<String> {
        actions::listed_failed_step(repo, account, job).await
    }
}
