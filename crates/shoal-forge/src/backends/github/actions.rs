//! GitHub Actions: check runs on a commit, the jobs their URLs link, and
//! the requests that rerun or cancel them.
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;

use crate::{
    ForgeRepo,
    ci::{CiAction, CiCheck, Job, JobState, Location, Operation, check, id},
};

/// GitHub's latest check runs.
pub(crate) fn checks(response: &Value) -> Result<Vec<CiCheck>> {
    let runs = response["check_runs"]
        .as_array()
        .context("GitHub check runs are not a list")?;
    ensure!(
        response["total_count"].as_u64() == Some(runs.len() as u64),
        "GitHub returned a partial page of check runs"
    );
    runs.iter()
        .map(|run| {
            let state = match (run["status"].as_str(), run["conclusion"].as_str()) {
                (
                    Some("completed"),
                    Some("failure" | "timed_out" | "cancelled" | "startup_failure"),
                ) => JobState::Failed,
                (Some("completed"), _) => JobState::Finished,
                (Some(_), _) => JobState::Running,
                (None, _) => bail!("GitHub check run has no status"),
            };
            check(run, "name", state, "details_url")
        })
        .collect()
}

/// The job a check run's URL links in the repository.
pub(crate) fn locate(repo: &ForgeRepo, target: &str) -> Option<Location> {
    let parts = repo.repository_path(target, "/actions/runs/")?;
    let [run, "job", job] = parts.as_slice() else {
        return None;
    };
    Some(Location::GitHub {
        run: id(run)?,
        job: id(job)?,
    })
}

/// The operation that applies `action` to `job`, and the checks it reaches.
/// A job rerun starts a new attempt of its run, which then refuses its
/// other jobs, so several failed jobs of one run rerun together with the
/// rest of its failed jobs. GitHub reruns only finished runs.
pub(crate) fn operation(
    jobs: &[Job],
    selected: &[Job],
    job: &Job,
    action: CiAction,
) -> Result<(Operation, Vec<String>)> {
    let location = job.location.context("check has no CI job")?;
    let in_run = |run| move |other: &&Job| github_run(other) == Some(run);
    let only = |operation| Ok((operation, vec![job.name.clone()]));
    if let (Location::GitHub { run, .. }, CiAction::Rerun) = (location, action) {
        // GitHub reruns only finished runs.
        let running = jobs
            .iter()
            .filter(in_run(run))
            .find(|other| other.state == JobState::Running);
        if let Some(running) = running {
            bail!(
                "check {} is still running in the same workflow run; wait for it or cancel it before rerunning {}",
                running.name,
                job.name
            );
        }
    }
    match (location, action) {
        (Location::GitHub { run, .. }, CiAction::Rerun)
            if selected.iter().filter(in_run(run)).count() > 1 =>
        {
            let failed = jobs
                .iter()
                .filter(in_run(run))
                .filter(|other| other.state == JobState::Failed);
            Ok((
                Operation::Post(format!("actions/runs/{run}/rerun-failed-jobs")),
                failed.map(|other| other.name.clone()).collect(),
            ))
        }
        (Location::GitHub { job: id, .. }, CiAction::Rerun) => {
            only(Operation::Post(format!("actions/jobs/{id}/rerun")))
        }
        (Location::GitHub { run, .. }, CiAction::Cancel) => {
            only(Operation::Post(format!("actions/runs/{run}/cancel")))
        }
        _ => bail!("check {} is not a GitHub Actions job", job.name),
    }
}

fn github_run(job: &Job) -> Option<u64> {
    match job.location {
        Some(Location::GitHub { run, .. }) => Some(run),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn check_runs_report_failed_and_running_checks() {
        let states = |checks: Vec<CiCheck>| checks.into_iter().map(|c| c.1).collect::<Vec<_>>();
        let github = json!({"total_count": 3, "check_runs": [
            {"name": "rust", "status": "completed", "conclusion": "timed_out", "details_url": "u"},
            {"name": "docs", "status": "queued", "conclusion": null},
            {"name": "lint", "status": "completed", "conclusion": "success"}
        ]});
        assert_eq!(
            states(checks(&github).unwrap()),
            [JobState::Failed, JobState::Running, JobState::Finished]
        );
        let partial = json!({"total_count": 4, "check_runs": github["check_runs"]});
        assert!(checks(&partial).is_err());
    }
}
