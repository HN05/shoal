//! Which runner took a job Shoal's runner claimed. Forgejo names a job's
//! runner only in the first line of its log, which `forgejo-runner` writes
//! as it takes the task: `Runner <UUID> (version:…) received task …`. A log
//! that does not start so names no runner yet.
use anyhow::Result;
use serde_json::Value;
use shoal_forge_types::{ForgeRepo, ci::runner::WaitingJob};

use crate::{
    backends::forgejo::actions::job_log,
    ci::{checks::plain, runner::JobTaker},
    transport::{
        account::Account,
        api::{Request, TransportRequests},
    },
};

/// The runner that took `job`.
pub(super) async fn job_taker(
    repo: &ForgeRepo,
    account: &Account,
    job: &WaitingJob,
) -> Result<JobTaker> {
    let read = Request::get(repo.repo_endpoint(&format!("actions/jobs/{}", job.id)));
    let value = repo.send(account, &read).await?;
    if let Some(taker) = untaken(&value, &job.handle) {
        return Ok(taker);
    }
    // The first step's log holds the line; a Forgejo that serves no steps
    // serves the whole log.
    let endpoint = repo.repo_endpoint(&format!("actions/jobs/{}/logs?step=0", job.id));
    let log = job_log(repo, account, &endpoint).await?;
    Ok(log_taker(&log))
}

/// What the job `value` tells of its taker without its log: none ever for
/// an attempt other than the one with `handle`, as a rerun starts, or for
/// one that ended without a task; none yet for one that waits.
fn untaken(value: &Value, handle: &str) -> Option<JobTaker> {
    if (value["handle"].as_str()).is_some_and(|latest| latest != handle) {
        return Some(JobTaker::Nobody);
    }
    if value["task_id"].as_u64().is_some_and(|task| task != 0) {
        return None;
    }
    let ended = matches!(
        value["status"].as_str(),
        Some("success" | "failure" | "cancelled" | "skipped")
    );
    Some(if ended {
        JobTaker::Nobody
    } else {
        JobTaker::Pending
    })
}

/// The runner the first line of the job log `log` names.
fn log_taker(log: &str) -> JobTaker {
    let log = plain::without_timestamps(log);
    let first = log.lines().next().unwrap_or_default();
    first
        .strip_prefix("Runner ")
        .and_then(|rest| rest.split_once(" (version:"))
        .map_or(JobTaker::Pending, |(uuid, _)| JobTaker::Runner(uuid.into()))
}

#[cfg(test)]
mod tests;
