//! What each forge does its own way with CI checks on a commit, the actions on
//! their jobs and their logs.
use anyhow::Result;

use crate::{
    ForgeRepo,
    account::Account,
    backends::{Backend, ForgeKind},
    capability::Capability,
    ci::{
        CiAction, CiCheck, Job, Location, Operation,
        checks::{CheckLog, plain},
    },
    numbers::JobId,
};

/// How the forge reads a commit's CI checks, acts on their jobs and
/// serves their logs.
#[shoal_forge_derive::forwarded(ForgeKind, dispatch)]
pub(crate) trait Checks: Backend {
    /// Each check's latest attempt on `head`.
    async fn ci_checks(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        head: &str,
    ) -> Result<Vec<CiCheck>>;

    /// The CI job a check's web URL links in the repository, if any.
    fn locate_job(&self, repo: &ForgeRepo, target: &str) -> Option<Location>;

    /// The operation that applies `action` to `job`, one of `selected` among
    /// the head's `jobs`, and the checks it reaches.
    fn ci_operation(
        &self,
        jobs: &[Job],
        selected: &[Job],
        job: &Job,
        action: CiAction,
    ) -> Result<(Operation, Vec<String>)>;

    /// The endpoint under the repository that carries out `operation`.
    async fn ci_endpoint(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _head: &str,
        operation: Operation,
    ) -> Result<String> {
        match operation {
            Operation::Post(endpoint) => Ok(endpoint),
            Operation::ForgejoRun { .. } | Operation::RerunForgejoJob { .. } => {
                anyhow::bail!("not a Forgejo repository")
            }
        }
    }

    /// The log of the failed check `name` on the current head of PR `url`.
    async fn failed_check_log(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        url: &str,
        name: &str,
    ) -> Result<CheckLog> {
        repo.failed_job_log(account, url, name).await
    }

    /// `log`, a job log as the forge serves it, as plain text.
    fn plain_log(&self, log: &str) -> String {
        plain::readable(log)
    }

    /// The step the plain job log `log` names as failed, where the forge's
    /// runner marks its steps.
    fn failed_step(&self, _log: &str) -> Option<String> {
        None
    }

    /// The step that failed in the job with API ID `job`, where the forge
    /// lists a job's steps with their states.
    async fn listed_failed_step(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _job: JobId,
    ) -> Option<String> {
        None
    }

    /// The endpoint under the repository that serves the plain-text log of
    /// the job at `location`.
    fn job_log_endpoint(&self, _location: Location) -> Result<String> {
        Err(crate::capability::unsupported(
            self,
            Capability::ReadJobLogs,
        ))
    }
}
