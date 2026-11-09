//! Forgejo's runner API and `forgejo-runner`: the job search, a run's event,
//! repository runners, and `forgejo-runner one-job` on the host backend.
use std::ffi::OsString;

use anyhow::{Context, Result};
use serde_json::Value;

use crate::{
    ForgeRepo,
    account::Account,
    ci::runner::{
        JobCommand, JobLaunch, JobRun, RegisteredRunner, Trigger, WaitingJob, file_url,
        registration,
    },
    numbers::{JobId, PullNumber, RunId, RunnerId},
    transport::api::{HttpMethod, Request, encode},
};

/// The program that runs one job.
pub(super) const PROGRAM: &str = "forgejo-runner";

/// The jobs the job search lists as waiting for a runner with `labels`.
pub(super) async fn waiting_jobs(
    repo: &ForgeRepo,
    account: &Account,
    labels: &[String],
) -> Result<Vec<WaitingJob>> {
    let read = Request::get(repo.repo_endpoint(&waiting_jobs_path(labels)));
    let listed = repo.send(account, &read).await?;
    listed
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|value| waiting_job(value).transpose())
        .collect()
}

/// The search for the jobs waiting for a runner with `labels`.
fn waiting_jobs_path(labels: &[String]) -> String {
    format!(
        "actions/runners/jobs?labels={}",
        encode(&labels.join(","), false)
    )
}

/// A listed job, unless it no longer waits.
fn waiting_job(value: &Value) -> Result<Option<WaitingJob>> {
    if value["status"]
        .as_str()
        .is_some_and(|status| status != "waiting")
    {
        return Ok(None);
    }
    let labels = value["runs_on"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|label| label.as_str().map(str::to_owned))
        .collect();
    Ok(Some(WaitingJob {
        handle: field(value, "handle")?,
        id: JobId::new(value["id"].as_u64().context("job has no ID")?),
        run: RunId::new(value["run_id"].as_u64().context("job has no run")?),
        name: field(value, "name")?,
        labels,
    }))
}

/// The run `value` with the event that started it.
pub(super) fn job_run(repo: &ForgeRepo, value: &Value) -> Result<JobRun> {
    let number = value["index_in_repo"]
        .as_u64()
        .context("run has no number")?;
    let payload: Value = value["event_payload"]
        .as_str()
        .and_then(|payload| serde_json::from_str(payload).ok())
        .unwrap_or_default();
    let pushed = payload["ref"]
        .as_str()
        .filter(|reference| reference.starts_with("refs/"));
    let (trigger, reference) = match (value["event"].as_str(), pushed) {
        (Some("pull_request"), _) => {
            let number = payload["pull_request"]["number"]
                .as_u64()
                .or_else(|| payload["number"].as_u64())
                .map(PullNumber::new)
                .context("the PR run names no PR")?;
            let reference = format!("refs/pull/{number}/head");
            (Trigger::Pull { number }, Some(reference))
        }
        (_, Some(reference)) => {
            let trigger = match reference.strip_prefix("refs/heads/") {
                Some(branch) => Trigger::Branch(branch.to_owned()),
                None => Trigger::Other,
            };
            (trigger, Some(reference.to_owned()))
        }
        (_, None) => (Trigger::Other, None),
    };
    Ok(JobRun {
        url: format!("{}/actions/runs/{number}", repo.repository_url()),
        workflow: value["workflow_id"].as_str().map(str::to_owned),
        commit: field(value, "commit_sha")?,
        trigger,
        reference,
    })
}

/// The request that registers a repository runner named `name`.
pub(super) fn register(repo: &ForgeRepo, name: &str) -> Request {
    registration(repo.repo_endpoint("actions/runners"), name)
}

pub(super) fn registered(value: &Value) -> Result<RegisteredRunner> {
    Ok(RegisteredRunner {
        id: value["id"]
            .as_u64()
            .map(RunnerId::new)
            .context("registered runner has no ID")?,
        uuid: field(value, "uuid")?,
        token: field(value, "token")?,
    })
}

pub(super) fn delete(repo: &ForgeRepo, id: RunnerId) -> Request {
    let endpoint = repo.repo_endpoint(&format!("actions/runners/{id}"));
    Request::new(HttpMethod::Delete, endpoint, None)
}

/// `forgejo-runner one-job` for the job `launch` names, each label on the
/// host backend, with a configuration that keeps the runner's files in the
/// launch's work directory and gives the job the launch's variables, which
/// `runner.envs` puts in every step's environment and the `env` context.
pub(super) fn job_command(repo: &ForgeRepo, launch: &JobLaunch<'_>) -> Result<JobCommand> {
    let runner = launch
        .runner
        .context("Forgejo runs a job only on the repository's runner")?;
    let work = launch
        .work_dir
        .to_str()
        .context("the job's work directory is not UTF-8")?;
    // JSON strings are YAML scalars, quoted however the path is spelt.
    let quoted = |path: String| serde_json::to_string(&path).expect("a string serializes");
    let envs: String = launch
        .env
        .iter()
        .map(|(name, value)| format!("    {}: {}\n", quoted(name.clone()), quoted(value.clone())))
        .collect();
    let config = format!(
        "log:\n  level: info\nrunner:\n  file: {}\n  capacity: 1\n  envs:\n{envs}host:\n  workdir_parent: {}\n",
        quoted(format!("{work}/.runner")),
        quoted(format!("{work}/host")),
    );
    let token_file = launch.work_dir.join("token");
    let config_file = launch.work_dir.join("config.yaml");
    let mut argv: Vec<OsString> = vec![
        launch.program.into(),
        "--config".into(),
        config_file.clone().into(),
        "one-job".into(),
        "--url".into(),
        repo.instance_url().into(),
        "--uuid".into(),
        runner.uuid.clone().into(),
        "--token-url".into(),
        file_url(&token_file)?,
        "--handle".into(),
        launch.job.handle.clone().into(),
    ];
    for label in launch.labels {
        argv.extend(["--label".into(), format!("{label}:host").into()]);
    }
    Ok(JobCommand {
        argv,
        files: vec![(token_file, runner.token.clone()), (config_file, config)],
        ..JobCommand::default()
    })
}

fn field(value: &Value, name: &str) -> Result<String> {
    value[name]
        .as_str()
        .map(str::to_owned)
        .with_context(|| format!("the forge's answer has no {name}"))
}

#[cfg(test)]
mod tests;
