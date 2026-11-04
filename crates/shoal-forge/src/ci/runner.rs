//! Running a repository's CI jobs on this machine: the jobs waiting for a
//! runner, the run each belongs to, registering and deleting the
//! repository's runner, and the command of the forge's own runner that runs
//! one job. Shoal decides which job runs when; the forge's runner runs its
//! steps.
use std::{ffi::OsString, path::Path};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{
    ForgeRepo,
    account::Account,
    backends::Backend,
    capability::Capability,
    transport::api::{HttpMethod, Request},
};

/// A job waiting in the forge's queue for a runner with its labels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitingJob {
    /// What claims this attempt of the job and no other.
    pub handle: String,
    /// The job's ID, which grows with each job the forge queues.
    pub id: u64,
    /// The API ID of the run it belongs to.
    pub run: u64,
    pub name: String,
    /// The labels it runs on.
    pub labels: Vec<String>,
}

/// What started a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    /// PR `number`, at its head when the run started.
    Pull { number: u64 },
    /// A push to, or a run started on, `branch`.
    Branch(String),
    /// Anything else, such as a tag or a schedule.
    Other,
}

/// The run a job belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobRun {
    pub url: String,
    /// Its workflow file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    /// The commit it runs on.
    pub commit: String,
    pub trigger: Trigger,
    /// The ref the repository's remote serves the commit under, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}

/// A runner the forge registered for the repository, with what it connects
/// as.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisteredRunner {
    /// The ID that deletes it.
    pub id: u64,
    pub uuid: String,
    pub token: String,
}

/// What one job's run of the forge's runner needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobLaunch<'a> {
    pub runner: &'a RegisteredRunner,
    /// A file holding the runner's token, so it stays off the command line.
    pub token_file: &'a Path,
    /// The file the command reads its configuration from.
    pub config_file: &'a Path,
    /// Where the runner keeps the job's own files.
    pub work_dir: &'a Path,
    pub labels: &'a [String],
    pub handle: &'a str,
    /// Variables every step and the workflow's `env` context get.
    pub env: &'a [(String, String)],
}

/// The command that runs one job, and the configuration it reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobCommand {
    pub argv: Vec<OsString>,
    pub config: String,
}

impl ForgeRepo {
    /// The program that runs one job, which must be on `PATH`, or why the
    /// forge cannot run jobs here.
    pub fn runner_program(&self) -> Result<&'static str> {
        self.kind.runner_program()
    }

    /// Refuse running jobs where the repository's server cannot.
    pub async fn require_runner(&self, account: &Account) -> Result<()> {
        self.require_on_server(account, Capability::RunJobs).await
    }

    /// The jobs waiting for a runner with `labels`, oldest first.
    pub async fn waiting_jobs(
        &self,
        account: &Account,
        labels: &[String],
    ) -> Result<Vec<WaitingJob>> {
        let path = self.kind.waiting_jobs_path(labels)?;
        let read = Request::get(self.repo_endpoint(&path));
        let listed = self.send(account, &read).await?;
        let mut jobs = listed
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter_map(|value| self.kind.waiting_job(value).transpose())
            .collect::<Result<Vec<_>>>()?;
        jobs.sort_by_key(|job| job.id);
        Ok(jobs)
    }

    /// The run with API ID `id`.
    pub async fn job_run(&self, account: &Account, id: u64) -> Result<JobRun> {
        let read = Request::get(self.repo_endpoint(&self.kind.job_run_path(id)?));
        let value = self.send(account, &read).await?;
        self.kind.job_run(self, &value)
    }

    /// Register a runner named `name` for this repository alone.
    pub async fn register_runner(&self, account: &Account, name: &str) -> Result<RegisteredRunner> {
        let request = self.kind.register_runner(self, name)?;
        let value = self.send(account, &request).await?;
        self.kind.registered_runner(&value)
    }

    /// Delete the repository's runner `id`; one already gone counts as deleted.
    pub async fn delete_runner(&self, account: &Account, id: u64) -> Result<()> {
        let request = self.kind.delete_runner(self, id)?;
        // A refusal's only possible status is that the runner is gone.
        let _gone = self.read_refusable(account, &request, &[404]).await?;
        Ok(())
    }

    /// The command that claims and runs the job `launch` names.
    pub fn job_command(&self, launch: &JobLaunch<'_>) -> Result<JobCommand> {
        self.kind.job_command(self, launch)
    }

    /// The web URL of the forge instance the repository is on.
    pub fn instance_url(&self) -> String {
        format!("{}://{}", self.web_scheme, self.host)
    }
}

/// The request that registers a runner at `endpoint` as `name`.
pub(crate) fn registration(endpoint: String, name: &str) -> Request {
    let body = serde_json::json!({
        "name": name,
        "description": "Shoal runs this repository's CI jobs on this machine",
    });
    Request::new(HttpMethod::Post, endpoint, Some(body))
}

/// The path of `file` as `forgejo-runner` reads a token URL.
pub(crate) fn file_url(file: &Path) -> Result<OsString> {
    let path = file
        .to_str()
        .context("the runner's token file is not UTF-8")?;
    Ok(format!("file:{path}").into())
}

#[cfg(test)]
mod tests;
