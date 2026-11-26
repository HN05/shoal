//! Running a repository's CI jobs on this machine. Shoal decides which job
//! runs when, and the forge's own runner runs its steps.
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    capability::{self, CapabilityRequests, Supports},
    ci::checks::CheckRequests,
    transport::{
        account::Account,
        api::{Request, TransportRequests},
    },
};
use shoal_forge_types::{
    ForgeRepo,
    backends::ForgeKind,
    capability::Capability,
    ci::runner::{JobRun, WaitingJob},
    numbers::{JobId, RunId, RunnerId},
    raw::HttpMethod,
};

/// How the forge registers runners on this machine and has them take
/// its jobs.
#[shoal_forge_derive::forwarded(ForgeKind, dispatch)]
pub(crate) trait Runners: Supports {
    /// The program that runs one job on this machine.
    fn runner_program(&self) -> Result<&'static str> {
        Err(capability::unsupported(self, Capability::RunJobs))
    }

    /// The jobs waiting for a runner with `labels`.
    async fn waiting_jobs(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _labels: &[String],
    ) -> Result<Vec<WaitingJob>> {
        Err(capability::unsupported(self, Capability::RunJobs))
    }

    /// The path under the repository that reads the run with API ID `id`.
    fn job_run_path(&self, _id: RunId) -> Result<String> {
        Err(capability::unsupported(self, Capability::RunJobs))
    }

    /// The run a read of its path returned as `value`.
    fn job_run(&self, _repo: &ForgeRepo, _value: &Value) -> Result<JobRun> {
        Err(capability::unsupported(self, Capability::RunJobs))
    }

    /// Register a runner named `name` for the repository, or, where the
    /// forge registers one per job, check that `account` may.
    async fn register_runner(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _name: &str,
    ) -> Result<Option<RegisteredRunner>> {
        Err(capability::unsupported(self, Capability::RunJobs))
    }

    /// The request that deletes the repository's runner `id`.
    fn delete_runner(&self, _repo: &ForgeRepo, _id: RunnerId) -> Result<Request> {
        Err(capability::unsupported(self, Capability::RunJobs))
    }

    /// The command that claims and runs the job `launch` names.
    async fn job_command(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _launch: &JobLaunch<'_>,
    ) -> Result<JobCommand> {
        Err(capability::unsupported(self, Capability::RunJobs))
    }

    /// The state of the runner `id`, or `None` once it is gone.
    async fn runner_state(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _id: RunnerId,
    ) -> Result<Option<RunnerState>> {
        Err(capability::unsupported(self, Capability::RunJobs))
    }

    /// The job of `run` that the runner `runner` took, if any.
    async fn runner_job(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _run: RunId,
        _runner: RunnerId,
    ) -> Result<Option<WaitingJob>> {
        Err(capability::unsupported(self, Capability::RunJobs))
    }

    /// The runner that took `job`, which a runner claims by its handle.
    async fn job_taker(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _job: &WaitingJob,
    ) -> Result<JobTaker> {
        Err(capability::unsupported(self, Capability::RunJobs))
    }

    /// Report the job `job`, which the repository's runner `runner` runs,
    /// cancelled as that runner, returning whether the forge has it
    /// cancelled. A forge whose runner protocol Shoal does not speak
    /// cancels none, and fails the job once its runner reports the stop.
    async fn cancel_runner_job(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _runner: Option<&RegisteredRunner>,
        _job: JobId,
    ) -> Result<bool> {
        Ok(false)
    }
}

/// A runner the forge registered for the repository, with what it connects
/// as. A forge whose runners each take one job registers none for the
/// repository, only one per job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisteredRunner {
    /// The ID that deletes it.
    pub id: RunnerId,
    pub uuid: String,
    pub token: String,
}

/// A registered runner as the forge reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerState {
    /// Whether it runs a job.
    pub busy: bool,
    /// The labels it takes jobs for, the forge's own included.
    pub labels: Vec<String>,
}

/// Which runner took a job, as the forge tells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobTaker {
    /// None yet: the job waits, or the runner that took it has not said so.
    Pending,
    /// The runner registered with this UUID.
    Runner(String),
    /// None ever will: the attempt claimed ended without a runner.
    Nobody,
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
    pub runner: Option<RunnerId>,
}

/// The entries of a runner's installation to copy into a directory of the
/// job's own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installation {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub entries: Vec<OsString>,
}

/// Running the forge's jobs on this machine.
#[shoal_forge_derive::extension(pub RunnerRequests)]
impl ForgeRepo {
    /// The program that runs one job, which must be on `PATH`, or why the
    /// forge cannot run jobs here.
    pub fn runner_program(&self) -> Result<&'static str> {
        self.kind().runner_program()
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
        let mut jobs = self.kind().waiting_jobs(self, account, labels).await?;
        jobs.sort_by_key(|job| job.id);
        Ok(jobs)
    }

    /// The run with API ID `id`.
    pub async fn job_run(&self, account: &Account, id: RunId) -> Result<JobRun> {
        let read = Request::get(self.repo_endpoint(&self.kind().job_run_path(id)?));
        let value = self.send(account, &read).await?;
        self.kind().job_run(self, &value)
    }

    /// Whether `run` is for a PR whose head moved past its commit.
    pub async fn run_superseded(&self, account: &Account, run: &JobRun) -> Result<bool> {
        let Some(number) = run.pull() else {
            return Ok(false);
        };
        let head = self.numbered_pull_head(account, number).await?;
        Ok(run.superseded_by(&head))
    }

    /// Register a runner named `name` for this repository alone, or, on a
    /// forge that registers one per job, check that `account` may.
    pub async fn register_runner(
        &self,
        account: &Account,
        name: &str,
    ) -> Result<Option<RegisteredRunner>> {
        self.kind().register_runner(self, account, name).await
    }

    /// Delete the repository's runner `id`; one already gone counts as deleted.
    pub async fn delete_runner(&self, account: &Account, id: RunnerId) -> Result<()> {
        let request = self.kind().delete_runner(self, id)?;
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
        self.kind().job_command(self, account, launch).await
    }

    /// The state of the runner `id`, or `None` once it is gone.
    pub async fn runner_state(
        &self,
        account: &Account,
        id: RunnerId,
    ) -> Result<Option<RunnerState>> {
        self.kind().runner_state(self, account, id).await
    }

    /// The job of the run with API ID `run` that the runner `runner` took,
    /// if it took one.
    pub async fn runner_job(
        &self,
        account: &Account,
        run: RunId,
        runner: RunnerId,
    ) -> Result<Option<WaitingJob>> {
        self.kind().runner_job(self, account, run, runner).await
    }

    /// The runner that took `job`, which a runner claims by its handle.
    pub async fn job_taker(&self, account: &Account, job: &WaitingJob) -> Result<JobTaker> {
        self.kind().job_taker(self, account, job).await
    }

    /// Have the forge cancel the job `job`, which the repository's runner
    /// `runner` runs, before that runner is stopped, so its report of the
    /// stop does not fail the job, returning whether the forge has it
    /// cancelled; the job's run goes on.
    pub async fn cancel_runner_job(
        &self,
        account: &Account,
        runner: Option<&RegisteredRunner>,
        job: JobId,
    ) -> Result<bool> {
        self.kind()
            .cancel_runner_job(self, account, runner, job)
            .await
    }
}

/// The file in a job's work directory where a runner that may take another
/// job than the one claimed names the run of the job it took, as it starts
/// it, before its first step.
const TOOK_RUN: &str = "took-run";
/// The file a runner's hook writes, before it reports the run, when it holds
/// the job back for the daemon; a job it let go on runs its steps already.
const AWAITS: &str = "awaits";
/// The file the daemon writes once the job's workspace is at the commit of
/// the run the runner took, and the one that says why it cannot be.
const PREPARED: &str = "prepared";
const REFUSED: &str = "refused";

/// What the runner of a job reported taking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TookRun {
    /// The run the job it took belongs to.
    pub run: RunId,
    /// Whether its hook holds the job back until the daemon answers, which
    /// it does for a job of another run than the claimed one.
    pub awaits: bool,
}

/// What the runner of the job with `work_dir` reported taking, unless it
/// has not started a job or was answered.
pub fn took_run(work_dir: &Path) -> Result<Option<TookRun>> {
    if [PREPARED, REFUSED]
        .iter()
        .any(|answer| work_dir.join(answer).exists())
    {
        return Ok(None);
    }
    reported_run(work_dir)
}

/// What the runner of the job with `work_dir` reported taking, answered or
/// not, unless it has not started a job.
pub fn reported_run(work_dir: &Path) -> Result<Option<TookRun>> {
    let reported = match std::fs::read_to_string(work_dir.join(TOOK_RUN)) {
        Ok(reported) => reported,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("read the run the runner took"),
    };
    let run = reported
        .trim()
        .parse()
        .with_context(|| format!("the runner took a job of an unknown run: {reported}"))?;
    Ok(Some(TookRun {
        run,
        awaits: work_dir.join(AWAITS).exists(),
    }))
}

/// Whether the runner of the job with `work_dir` reported taking a job.
pub fn took_job(work_dir: &Path) -> bool {
    work_dir.join(TOOK_RUN).exists()
}

/// Let the runner of the job with `work_dir` go on with the job it took,
/// or fail it saying why its workspace could not move to the job's commit.
pub fn answer_took_run(work_dir: &Path, answer: Result<(), String>) -> Result<()> {
    let (file, contents) = match answer {
        Ok(()) => (PREPARED, String::new()),
        Err(reason) => (REFUSED, reason),
    };
    let path = work_dir.join(file);
    let partial = work_dir.join(format!("{file}.partial"));
    std::fs::write(&partial, contents).context("answer the runner")?;
    std::fs::rename(&partial, &path).context("answer the runner")
}

/// A `sh` script for a runner's job-started hook that runs `before`, reports
/// the run whose job it took and goes on at once with a job of `run`, whose
/// commit the workspace has; with another run's job it waits for the daemon
/// to move the workspace, failing the job when the daemon refuses or does
/// not answer within ten minutes.
pub(crate) fn took_run_hook(work_dir: &Path, run: RunId, before: &str) -> Result<String> {
    let quoted = |name: &str| shell_quote(&work_dir.join(name));
    let (took, awaits, prepared, refused) = (
        quoted(TOOK_RUN)?,
        quoted(AWAITS)?,
        quoted(PREPARED)?,
        quoted(REFUSED)?,
    );
    Ok(format!(
        "#!/bin/sh\n{before}\
[ \"$GITHUB_RUN_ID\" = {run} ] || : > {awaits} || exit 1\n\
printf '%s\\n' \"$GITHUB_RUN_ID\" > {took}.partial && mv {took}.partial {took} || exit 1\n\
[ \"$GITHUB_RUN_ID\" = {run} ] && exit 0\n\
echo \"Shoal moves this job's workspace to the commit of run $GITHUB_RUN_ID\"\n\
waited=0\n\
while [ \"$waited\" -lt 600 ]; do\n\
[ -e {prepared} ] && exit 0\n\
if [ -e {refused} ]; then cat {refused}; exit 1; fi\n\
sleep 1\n\
waited=$((waited + 1))\n\
done\n\
echo 'Shoal did not move the workspace within ten minutes'\n\
exit 1\n"
    ))
}

/// `path` as one `sh` word.
fn shell_quote(path: &Path) -> Result<String> {
    let path = path.to_str().context("a path of the job is not UTF-8")?;
    Ok(quote(path))
}

/// `text` as one `sh` word.
pub(crate) fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
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
