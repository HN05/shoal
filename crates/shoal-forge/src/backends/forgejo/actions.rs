//! Forgejo Actions: the runs that commit statuses link, and the logs of
//! failed jobs.
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use tokio::process::Command;

use crate::{
    ForgeRepo,
    account::Account,
    backends::Backend,
    ci::{
        JobState,
        checks::{CheckLog, MAX_LOG_BYTES, NoJobLog},
    },
    items::link::ItemKind,
    transport::{
        api::{Request, TIMEOUT, curl_body},
        programs,
    },
};

/// The log of the failed check `name` on the current head of PR `url`.
pub(crate) async fn failed_check_log(
    repo: &ForgeRepo,
    account: &Account,
    url: &str,
    name: &str,
) -> Result<CheckLog> {
    let (number, _) = repo.pull(url)?;
    let pull = repo
        .send(
            account,
            &Request::get(repo.item_endpoint(ItemKind::Pr, number)),
        )
        .await?;
    let head = pull["head"]["sha"]
        .as_str()
        .context("PR has no head commit")?;
    let statuses = repo
        .pages(
            account,
            &repo.repo_endpoint(&format!("commits/{head}/statuses")),
        )
        .await?;
    // Forgejo lists newest statuses first, including earlier attempts.
    let status = statuses
        .iter()
        .find(|status| status["context"].as_str() == Some(name))
        .context("check was not found on the PR's current head")?;
    ensure!(
        matches!(status["status"].as_str(), Some("failure" | "error")),
        "check has not failed"
    );
    let no_log = || NoJobLog { name: name.into() };
    let target = status["target_url"].as_str().ok_or_else(no_log)?;
    let (run_number, job_index) = job_location(repo, target).map_err(|_| no_log())?;
    let run_id = run_id_for(repo, account, run_number, head).await?;
    let jobs = repo
        .send(
            account,
            &Request::get(repo.repo_endpoint(&format!("actions/runs/{run_id}/jobs"))),
        )
        .await?;
    let job_id = job_id(&jobs, job_index, run_id)?;
    let endpoint = repo.repo_endpoint(&format!("actions/jobs/{job_id}/logs"));
    let log = job_log(repo, account, &endpoint).await?;
    let log = repo.kind.plain_log(&log);
    let step = failed_step(&log);
    Ok(CheckLog::from_text(url, name, &log, step).with_job(job_url(repo, target)))
}

/// The step the runner marks as failing, or the one it started last before
/// the job's failure when that step did not succeed. A job log may mark only
/// its first steps, so a step that succeeded says nothing about which later
/// one failed.
pub(crate) fn failed_step(log: &str) -> Option<String> {
    let mut current = None;
    for line in log.lines() {
        if let Some((_, step)) = line.split_once("⭐ Run ") {
            current = Some(step.trim().chars().take(256).collect());
        }
        if line.contains("✅  Success - ") {
            current = None;
        }
        if let Some((_, step)) = line.split_once("❌  Failure - ") {
            return Some(step.trim().chars().take(256).collect());
        }
        if line.contains("[runner]:") && line.contains("failure") {
            return current;
        }
    }
    None
}

/// Forgejo links a job by its path on the host.
fn job_url(repo: &ForgeRepo, target: &str) -> String {
    if target.starts_with('/') {
        format!("{}://{}{target}", repo.web_scheme, repo.host)
    } else {
        target.into()
    }
}

/// The API ID of the run numbered `number` in the repository, which must
/// have run on `head`.
pub(crate) async fn run_id_for(
    repo: &ForgeRepo,
    account: &Account,
    number: u64,
    head: &str,
) -> Result<u64> {
    let runs = repo
        .send(
            account,
            &Request::get(repo.repo_endpoint(&format!("actions/runs?run_number={number}"))),
        )
        .await?;
    run_id(&runs, number, head)
}

/// The state of a run or job from its status.
pub(crate) fn state(status: &Value) -> Result<JobState> {
    Ok(match status.as_str() {
        Some("waiting" | "running" | "blocked" | "unknown") => JobState::Running,
        Some("failure" | "cancelled") => JobState::Failed,
        Some("success" | "skipped") => JobState::Finished,
        status => anyhow::bail!("unknown Forgejo Actions status {status:?}"),
    })
}

/// The one run a list filtered by `number` holds.
pub(crate) fn numbered_run(runs: &Value, number: u64) -> Result<&Value> {
    let runs = runs["workflow_runs"]
        .as_array()
        .context("Actions runs are not a list")?;
    let [run] = runs.as_slice() else {
        anyhow::bail!("Actions run number did not identify one run")
    };
    ensure!(
        run["index_in_repo"] == number,
        "Actions run does not match its number"
    );
    Ok(run)
}

/// The run number of a run's or job's web URL in the repository, which may
/// be relative to the host.
pub(crate) fn run_number(repo: &ForgeRepo, target: &str) -> Option<u64> {
    let parts = runs_path(repo, target)?;
    parts.first()?.parse().ok().filter(|number| *number > 0)
}

/// The path components of `target` after the repository's runs. A host may
/// link relative to itself, or over HTTPS while its remote uses HTTP.
fn runs_path<'a>(repo: &ForgeRepo, target: &'a str) -> Option<Vec<&'a str>> {
    let relative = ["https", "http"]
        .iter()
        .find_map(|scheme| target.strip_prefix(&format!("{scheme}://{}", repo.host)))
        .unwrap_or(target);
    let rest = relative.strip_prefix(&format!("/{}/actions/runs/", repo.path))?;
    Some(rest.split('/').collect())
}

/// Web URLs use a repository run number and zero-based job index, neither
/// API ID.
pub(crate) fn job_location(repo: &ForgeRepo, target: &str) -> Result<(u64, usize)> {
    let parts =
        runs_path(repo, target).context("check does not link to this repository's Actions job")?;
    let [run, "jobs", job] = parts.as_slice() else {
        anyhow::bail!("invalid Actions job URL")
    };
    let run = run.parse::<u64>().context("invalid Actions run number")?;
    let job = job.parse::<usize>().context("invalid Actions job index")?;
    ensure!(run > 0, "invalid Actions run number");
    Ok((run, job))
}

/// Read plaintext logs without following redirects or exposing the login.
pub(crate) async fn job_log(repo: &ForgeRepo, account: &Account, endpoint: &str) -> Result<String> {
    let authorization = account.authorization(repo).await?;
    let url = format!("{}://{}/api/v1/{endpoint}", repo.web_scheme, repo.host);
    let mut curl = Command::new(programs::CURL);
    curl.args([
        "-q",
        "-sS",
        "--max-time",
        "30",
        "--max-filesize",
        &MAX_LOG_BYTES.to_string(),
        "-H",
        "Accept: text/plain",
        "-w",
        "\n%{http_code}",
        "-K",
        "-",
        &url,
    ]);
    let output = shoal_subprocess::Run::new(curl)
        .input(authorization.unwrap_or_default())
        .timeout(TIMEOUT)
        // curl appends a newline and a three-digit HTTP status.
        .output_limit(MAX_LOG_BYTES + 4)
        .output()
        .await
        .context("fetch Forgejo job log (maximum 8 MiB)")?;
    Ok(curl_body(&output)?.to_owned())
}

fn run_id(runs: &Value, number: u64, head: &str) -> Result<u64> {
    let run = numbered_run(runs, number)?;
    ensure!(
        run["commit_sha"] == head,
        "Actions run does not match the PR check's head"
    );
    run["id"]
        .as_u64()
        .filter(|id| *id > 0)
        .context("Actions run has no ID")
}

fn job_id(jobs: &Value, index: usize, run: u64) -> Result<u64> {
    let job = jobs
        .as_array()
        .and_then(|jobs| jobs.get(index))
        .context("Actions job index was not found")?;
    ensure!(
        job["run_id"] == run,
        "Actions job does not belong to the check's run"
    );
    ensure!(job["status"] == "failure", "Actions job has not failed");
    job["id"]
        .as_u64()
        .filter(|id| *id > 0)
        .context("Actions job has no ID")
}

#[cfg(test)]
mod tests;
