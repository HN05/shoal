//! Running a repository's CI jobs on this machine: the jobs waiting for a
//! runner, the run each belongs to, registering and deleting the
//! repository's runner, and the command of the forge's own runner that runs
//! one job. Shoal decides which job runs when; the forge's runner runs its
//! steps.
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

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
/// as. A forge whose runners each take one job registers none for the
/// repository, only one per job.
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
    /// The repository's runner, on a forge that keeps one.
    pub runner: Option<&'a RegisteredRunner>,
    /// The runner's program, as found on `PATH`.
    pub program: &'a Path,
    /// The name a runner registered for this job alone takes.
    pub name: &'a str,
    /// Where the runner keeps the job's own files.
    pub work_dir: &'a Path,
    /// The job's workspace, at the job's commit.
    pub workspace: &'a Path,
    pub labels: &'a [String],
    pub job: &'a WaitingJob,
    /// Variables every step and the workflow's `env` context get.
    pub env: &'a [(String, String)],
}

/// The command that runs one job, and what it needs in place before it
/// starts.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct JobCommand {
    pub argv: Vec<OsString>,
    /// Variables the command gets besides its workspace's, which may be
    /// secret.
    pub env: Vec<(String, String)>,
    /// Files to write, readable by the user alone, such as the runner's
    /// configuration and credentials.
    pub files: Vec<(PathBuf, String)>,
    /// The runner's installation, copied for a runner that writes its
    /// credentials and work into its own directory.
    pub installation: Option<Installation>,
    /// Where the runner expects the job's working directory, made a link to
    /// the job's workspace once the installation is in place.
    pub workspace_links: Vec<PathBuf>,
    /// The runner registered for this job alone, which ends with its job.
    pub runner: Option<u64>,
}

/// The entries of a runner's installation to copy into a directory of the
/// job's own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installation {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub entries: Vec<OsString>,
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
        let mut jobs = self.kind.waiting_jobs(self, account, labels).await?;
        jobs.sort_by_key(|job| job.id);
        Ok(jobs)
    }

    /// The run with API ID `id`.
    pub async fn job_run(&self, account: &Account, id: u64) -> Result<JobRun> {
        let read = Request::get(self.repo_endpoint(&self.kind.job_run_path(id)?));
        let value = self.send(account, &read).await?;
        self.kind.job_run(self, &value)
    }

    /// Register a runner named `name` for this repository alone, or, on a
    /// forge that registers one per job, check that `account` may.
    pub async fn register_runner(
        &self,
        account: &Account,
        name: &str,
    ) -> Result<Option<RegisteredRunner>> {
        self.kind.register_runner(self, account, name).await
    }

    /// Delete the repository's runner `id`; one already gone counts as deleted.
    pub async fn delete_runner(&self, account: &Account, id: u64) -> Result<()> {
        let request = self.kind.delete_runner(self, id)?;
        // A refusal's only possible status is that the runner is gone.
        let _gone = self.read_refusable(account, &request, &[404]).await?;
        Ok(())
    }

    /// The command that claims and runs the job `launch` names, registering
    /// a runner for it alone where the forge's runners take one job each.
    pub async fn job_command(
        &self,
        account: &Account,
        launch: &JobLaunch<'_>,
    ) -> Result<JobCommand> {
        self.kind.job_command(self, account, launch).await
    }

    /// Whether the runner `id` runs a job, or `None` once it is gone.
    pub async fn runner_busy(&self, account: &Account, id: u64) -> Result<Option<bool>> {
        self.kind.runner_busy(self, account, id).await
    }

    /// The job of the run with API ID `run` that the runner `runner` took,
    /// if it took one.
    pub async fn runner_job(
        &self,
        account: &Account,
        run: u64,
        runner: u64,
    ) -> Result<Option<WaitingJob>> {
        self.kind.runner_job(self, account, run, runner).await
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
