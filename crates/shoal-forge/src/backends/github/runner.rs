//! GitHub's self-hosted runners and `actions/runner`: the queued jobs of the
//! repository's active runs, a run's event, and a just-in-time runner for
//! each job, which takes one job its labels match and ends with it. GitHub
//! picks that job, so a job-started hook reports the run it belongs to and
//! waits for the daemon when it is not the claimed one's.
use std::{ffi::OsString, path::Path};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::{
    ForgeRepo,
    account::Account,
    ci::runner::{
        Installation, JobCommand, JobLaunch, JobRun, Trigger, WaitingJob, quote, took_run_hook,
    },
    items::item::text,
    transport::api::{HttpMethod, Request},
};

/// The program `actions/runner` runs each job with; its installation is the
/// directory above the one that holds it.
pub(super) const PROGRAM: &str = "Runner.Listener";

/// The runner group every repository runner joins.
const DEFAULT_GROUP: u64 = 1;

/// The run states whose runs may have queued jobs.
const ACTIVE_RUNS: [&str; 2] = ["queued", "in_progress"];

/// The queued jobs of the repository's active runs that a runner with
/// `labels` may take: GitHub gives a job to a runner that has each of the
/// labels it runs on, whatever their case.
pub(super) async fn waiting_jobs(
    repo: &ForgeRepo,
    account: &Account,
    labels: &[String],
) -> Result<Vec<WaitingJob>> {
    let mut jobs = Vec::new();
    for status in ACTIVE_RUNS {
        let endpoint = repo.repo_endpoint(&format!("actions/runs?status={status}&per_page=100"));
        let listed = repo.send(account, &Request::get(endpoint)).await?;
        for run in listed["workflow_runs"].as_array().into_iter().flatten() {
            let run = run["id"].as_u64().context("Actions run has no ID")?;
            let endpoint = repo.repo_endpoint(&format!(
                "actions/runs/{run}/jobs?filter=latest&per_page=100"
            ));
            let listed = repo.send(account, &Request::get(endpoint)).await?;
            for job in listed["jobs"].as_array().into_iter().flatten() {
                if let Some(job) = queued_job(job)?
                    && matches(&job.labels, labels)
                {
                    jobs.push(job);
                }
            }
        }
    }
    Ok(jobs)
}

/// A listed job, unless it no longer waits for a runner.
fn queued_job(value: &Value) -> Result<Option<WaitingJob>> {
    if value["status"] != "queued" {
        return Ok(None);
    }
    let id = value["id"].as_u64().context("Actions job has no ID")?;
    Ok(Some(WaitingJob {
        handle: id.to_string(),
        id,
        run: value["run_id"].as_u64().context("Actions job has no run")?,
        name: text(&value["name"]).context("Actions job has no name")?,
        labels: names(&value["labels"]),
    }))
}

/// Whether a runner with `labels` may take a job that runs on `wanted`.
fn matches(wanted: &[String], labels: &[String]) -> bool {
    !wanted.is_empty()
        && wanted
            .iter()
            .all(|label| labels.iter().any(|own| own.eq_ignore_ascii_case(label)))
}

fn names(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|label| label.as_str().map(str::to_owned))
        .collect()
}

/// The run `value` with the event that started it. A PR's run names the PR
/// only when it comes from the repository itself; a fork's, like a tag's or
/// a schedule's, is fetched by its commit.
pub(super) fn job_run(value: &Value) -> Result<JobRun> {
    let event = value["event"].as_str().unwrap_or_default();
    let pull = value["pull_requests"][0]["number"].as_u64();
    let branch = text(&value["head_branch"]);
    let (trigger, reference) = match (event, pull, branch) {
        ("pull_request" | "pull_request_target", Some(number), _) => (
            Trigger::Pull { number },
            Some(format!("refs/pull/{number}/head")),
        ),
        ("push" | "workflow_dispatch", _, Some(branch)) => (Trigger::Branch(branch), None),
        _ => (Trigger::Other, None),
    };
    Ok(JobRun {
        url: text(&value["html_url"]).context("Actions run has no URL")?,
        workflow: text(&value["path"])
            .map(|path| path.rsplit('/').next().unwrap_or_default().to_owned()),
        commit: text(&value["head_sha"]).context("Actions run has no commit")?,
        trigger,
        reference,
    })
}

/// Read the repository's runners, which only an account that may register
/// them can.
pub(super) async fn check_registration(repo: &ForgeRepo, account: &Account) -> Result<()> {
    let endpoint = repo.repo_endpoint("actions/runners?per_page=1");
    repo.send(account, &Request::get(endpoint))
        .await
        .context("the forge login cannot administer the repository's runners")?;
    Ok(())
}

pub(super) fn delete(repo: &ForgeRepo, id: u64) -> Request {
    let endpoint = repo.repo_endpoint(&format!("actions/runners/{id}"));
    Request::new(HttpMethod::Delete, endpoint, None)
}

/// Whether the runner `id` runs a job, or `None` once it is gone.
pub(super) async fn busy(repo: &ForgeRepo, account: &Account, id: u64) -> Result<Option<bool>> {
    let read = Request::get(repo.repo_endpoint(&format!("actions/runners/{id}")));
    Ok(match repo.read_refusable(account, &read, &[404]).await? {
        Ok(runner) => Some(runner["busy"] == true),
        Err(_) => None,
    })
}

/// The job of `run` that the runner `runner` took.
pub(super) async fn runner_job(
    repo: &ForgeRepo,
    account: &Account,
    run: u64,
    runner: u64,
) -> Result<Option<WaitingJob>> {
    let endpoint = repo.repo_endpoint(&format!(
        "actions/runs/{run}/jobs?filter=latest&per_page=100"
    ));
    let listed = repo.send(account, &Request::get(endpoint)).await?;
    let Some(job) = listed["jobs"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|job| job["runner_id"].as_u64() == Some(runner))
    else {
        return Ok(None);
    };
    let id = job["id"].as_u64().context("Actions job has no ID")?;
    Ok(Some(WaitingJob {
        handle: id.to_string(),
        id,
        run,
        name: text(&job["name"]).context("Actions job has no name")?,
        labels: names(&job["labels"]),
    }))
}

/// Register a just-in-time runner named for the job `launch` claims and
/// run it from a copy of the installation in the job's work directory,
/// where it writes its credentials and work: its working directory links to
/// the job's workspace, its configuration arrives through the environment,
/// off the command line, and its job-started hook gives every step the
/// launch's variables and reports the run of the job GitHub gave it.
pub(super) async fn job_command(
    repo: &ForgeRepo,
    account: &Account,
    launch: &JobLaunch<'_>,
) -> Result<JobCommand> {
    let installation = installation(launch)?;
    let hook = launch.work_dir.join("job-started.sh");
    let script = took_run_hook(launch.work_dir, launch.job.run, &job_env_lines(launch.env))?;
    let request = Request::new(
        HttpMethod::Post,
        repo.repo_endpoint("actions/runners/generate-jitconfig"),
        Some(json!({
            "name": launch.name,
            "runner_group_id": DEFAULT_GROUP,
            "labels": launch.labels,
            "work_folder": "_work",
        })),
    );
    let registered = repo.send(account, &request).await?;
    let runner = registered["runner"]["id"]
        .as_u64()
        .context("the registered runner has no ID")?;
    let config = text(&registered["encoded_jit_config"])
        .context("the registered runner has no configuration")?;
    let name = repo
        .path
        .rsplit('/')
        .next()
        .context("the repository has no name")?;
    let destination = &installation.destination;
    Ok(JobCommand {
        argv: vec![destination.join("run.sh").into()],
        env: vec![
            ("ACTIONS_RUNNER_INPUT_JITCONFIG".into(), config),
            (
                "ACTIONS_RUNNER_HOOK_JOB_STARTED".into(),
                hook.to_str()
                    .context("the job's hook path is not UTF-8")?
                    .into(),
            ),
        ],
        files: vec![(hook, script)],
        workspace_links: vec![destination.join("_work").join(name).join(name)],
        runner: Some(runner),
        installation: Some(installation),
    })
}

/// The installation `launch.program` belongs to, without the files and
/// directories a runner writes into it: its configuration, which begins
/// with a dot, its work and logs, and the versions an update replaced.
fn installation(launch: &JobLaunch<'_>) -> Result<Installation> {
    let program = std::fs::canonicalize(launch.program)
        .with_context(|| format!("resolve {}", launch.program.display()))?;
    let source = program
        .parent()
        .and_then(Path::parent)
        .context("the runner's program is not in its installation's bin directory")?
        .to_owned();
    let mut entries: Vec<OsString> = std::fs::read_dir(&source)
        .with_context(|| format!("read {}", source.display()))?
        .map(|entry| Ok(entry?.file_name()))
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .filter(installed)
        .collect();
    entries.sort();
    Ok(Installation {
        source,
        destination: launch.work_dir.join("runner"),
        entries,
    })
}

fn installed(name: &OsString) -> bool {
    let name = name.to_string_lossy();
    !name.starts_with('.')
        && !["_work", "_diag"].contains(&name.as_ref())
        && !name.starts_with("bin.")
        && !name.starts_with("externals.")
}

/// Lines of a `sh` script that add `env` to the variables of the job's later
/// steps and its `env` context.
fn job_env_lines(env: &[(String, String)]) -> String {
    env.iter()
        .map(|(name, value)| {
            format!(
                "printf '%s=%s\\n' {} {} >> \"$GITHUB_ENV\" || exit 1\n",
                quote(name),
                quote(value)
            )
        })
        .collect()
}

#[cfg(test)]
mod tests;
