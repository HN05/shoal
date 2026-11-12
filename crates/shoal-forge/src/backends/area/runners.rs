//! What each forge does its own way with runners on this machine and the
//! jobs they take.
use anyhow::Result;
use serde_json::Value;

use crate::{
    ForgeRepo,
    account::Account,
    backends::{Backend, ForgeKind},
    capability::Capability,
    ci::runner::{JobCommand, JobLaunch, JobRun, RegisteredRunner, RunnerState, WaitingJob},
    numbers::{RunId, RunnerId},
    transport::api::Request,
};

/// How the forge registers runners on this machine and has them take
/// its jobs.
#[shoal_forge_derive::forwarded(ForgeKind, dispatch)]
pub(crate) trait Runners: Backend {
    /// The program that runs one job on this machine.
    fn runner_program(&self) -> Result<&'static str> {
        Err(crate::capability::unsupported(self, Capability::RunJobs))
    }

    /// The jobs waiting for a runner with `labels`.
    async fn waiting_jobs(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _labels: &[String],
    ) -> Result<Vec<WaitingJob>> {
        Err(crate::capability::unsupported(self, Capability::RunJobs))
    }

    /// The path under the repository that reads the run with API ID `id`.
    fn job_run_path(&self, _id: RunId) -> Result<String> {
        Err(crate::capability::unsupported(self, Capability::RunJobs))
    }

    /// The run a read of its path returned as `value`.
    fn job_run(&self, _repo: &ForgeRepo, _value: &Value) -> Result<JobRun> {
        Err(crate::capability::unsupported(self, Capability::RunJobs))
    }

    /// Register a runner named `name` for the repository, or, where the
    /// forge registers one per job, check that `account` may.
    async fn register_runner(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _name: &str,
    ) -> Result<Option<RegisteredRunner>> {
        Err(crate::capability::unsupported(self, Capability::RunJobs))
    }

    /// The request that deletes the repository's runner `id`.
    fn delete_runner(&self, _repo: &ForgeRepo, _id: RunnerId) -> Result<Request> {
        Err(crate::capability::unsupported(self, Capability::RunJobs))
    }

    /// The command that claims and runs the job `launch` names.
    async fn job_command(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _launch: &JobLaunch<'_>,
    ) -> Result<JobCommand> {
        Err(crate::capability::unsupported(self, Capability::RunJobs))
    }

    /// The state of the runner `id`, or `None` once it is gone.
    async fn runner_state(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _id: RunnerId,
    ) -> Result<Option<RunnerState>> {
        Err(crate::capability::unsupported(self, Capability::RunJobs))
    }

    /// The job of `run` that the runner `runner` took, if any.
    async fn runner_job(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _run: RunId,
        _runner: RunnerId,
    ) -> Result<Option<WaitingJob>> {
        Err(crate::capability::unsupported(self, Capability::RunJobs))
    }
}
