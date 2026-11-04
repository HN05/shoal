//! GitHub's self-hosted runners: the queued jobs of the repository's active
//! runs, a run's event, and the repository's runners.
use anyhow::{Context, Result};
use serde_json::Value;

use crate::{
    ForgeRepo,
    account::Account,
    ci::runner::{JobRun, Trigger, WaitingJob},
    items::item::text,
    transport::api::{HttpMethod, Request},
};

/// The program `actions/runner` runs each job with; its installation is the
/// directory above the one that holds it.
pub(super) const PROGRAM: &str = "Runner.Listener";

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

#[cfg(test)]
mod tests;
