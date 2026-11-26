//! Reporting a job cancelled through Forgejo's runner protocol, as the
//! runner Shoal registered for the repository. Forgejo keeps a finished
//! task's result, so the runner's own report of the stop that follows
//! changes nothing.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use shoal_forge_types::{ForgeRepo, numbers::JobId, raw::HttpMethod};
use shoal_time::UnixTime;

use crate::{
    ci::runner::RegisteredRunner,
    transport::{
        account::Account,
        api::{Request, TransportRequests, json_body},
    },
};

/// The result that ends a task cancelled.
const CANCELLED: &str = "RESULT_CANCELLED";

/// Report the job `job`, which `runner` runs, cancelled, returning whether
/// Forgejo has it cancelled rather than finished some other way first.
pub(super) async fn cancel_job(
    repo: &ForgeRepo,
    account: &Account,
    runner: &RegisteredRunner,
    job: JobId,
) -> Result<bool> {
    let task = job_task(repo, account, job).await?;
    let stopped = (UnixTime::now().utc_rfc3339()).context("the time could not be written")?;
    let state = json!({"state": {"id": task, "result": CANCELLED, "stoppedAt": stopped}});
    let answer = send(repo, runner, "UpdateTask", state).await?;
    Ok(answer["state"]["result"] == CANCELLED)
}

/// The task running the job `job`, which Forgejo reads from release 17.
async fn job_task(repo: &ForgeRepo, account: &Account, job: JobId) -> Result<u64> {
    let read = Request::get(repo.repo_endpoint(&format!("actions/jobs/{job}")));
    let value = repo.send(account, &read).await?;
    (value["task_id"].as_u64())
        .filter(|task| *task != 0)
        .with_context(|| format!("no runner took job {job}"))
}

/// The answer to the runner service's `method` with `body`, sent as
/// `runner`, whose credentials go to curl on stdin.
async fn send(
    repo: &ForgeRepo,
    runner: &RegisteredRunner,
    method: &str,
    body: Value,
) -> Result<Value> {
    let url = service_url(repo, method);
    let request = Request::new(HttpMethod::Post, url.clone(), Some(body));
    async {
        let response = (repo.curl_send(&url, Some(credentials(runner)?), &request)).await?;
        json_body(response.success_body()?)
    }
    .await
    .with_context(|| format!("POST {url}"))
}

/// The URL of the runner service's `method`.
fn service_url(repo: &ForgeRepo, method: &str) -> String {
    format!(
        "{}/api/actions/runner.v1.RunnerService/{method}",
        repo.instance_url()
    )
}

/// The curl config lines that identify a request as from `runner`, which
/// refuse credentials that would not stay on their line.
fn credentials(runner: &RegisteredRunner) -> Result<Vec<u8>> {
    let plain = |value: &str| {
        !value.is_empty()
            && (value.chars()).all(|c| c.is_ascii_graphic() && !matches!(c, '"' | '\\'))
    };
    ensure!(
        plain(&runner.uuid) && plain(&runner.token),
        "the runner's UUID or token has an unexpected format"
    );
    Ok(format!(
        "header = \"x-runner-uuid: {}\"\nheader = \"x-runner-token: {}\"\n",
        runner.uuid, runner.token
    )
    .into_bytes())
}

#[cfg(test)]
mod tests;
