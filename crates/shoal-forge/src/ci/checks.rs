//! Failed Actions logs resolved from a PR's current check, using its agent account.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    ForgeKind, ForgeRepo, account::Account, items::link::ItemKind, transport::api::Request,
};

/// The most of a log's end a [`CheckLog`] keeps, in bytes.
pub const TAIL_BYTES: usize = 8192;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckLog {
    pub url: String,
    pub name: String,
    pub text: String,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_step: Option<String>,
}

/// Refuse a check name no forge shows, before any lookup.
pub fn validate_check_name(name: &str) -> Result<()> {
    ensure!(
        !name.trim().is_empty() && name.len() <= 1024,
        "check name must be nonempty and at most 1024 bytes"
    );
    Ok(())
}

impl ForgeRepo {
    pub fn supports_check_logs(&self) -> bool {
        self.kind == ForgeKind::Forgejo
    }

    pub async fn failed_check_log(
        &self,
        account: &Account,
        url: &str,
        name: &str,
    ) -> Result<CheckLog> {
        ensure!(
            self.kind == ForgeKind::Forgejo,
            "check logs currently require Forgejo Actions"
        );
        let (number, _) = self.pull(url)?;
        let pull = self
            .send(
                account,
                &Request::get(self.item_endpoint(ItemKind::Pr, number)),
            )
            .await?;
        let head = pull["head"]["sha"]
            .as_str()
            .context("PR has no head commit")?;
        let statuses = self
            .pages(
                account,
                &self.repo_endpoint(&format!("commits/{head}/statuses")),
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
        let target = status["target_url"]
            .as_str()
            .context("check has no Actions job URL")?;
        let (run_number, job_index) = self.job_location(target)?;
        let run_id = self.forgejo_run_id(account, run_number, head).await?;
        let jobs = self
            .send(
                account,
                &Request::get(self.repo_endpoint(&format!("actions/runs/{run_id}/jobs"))),
            )
            .await?;
        let job_id = job_id(&jobs, job_index, run_id)?;
        let log = self
            .forgejo_log(
                account,
                &self.repo_endpoint(&format!("actions/jobs/{job_id}/logs")),
            )
            .await?;
        Ok(CheckLog::from_text(url, name, &log))
    }

    /// The API ID of the run numbered `number` in the repository, which must
    /// have run on `head`.
    pub(crate) async fn forgejo_run_id(
        &self,
        account: &Account,
        number: u64,
        head: &str,
    ) -> Result<u64> {
        let runs = self
            .send(
                account,
                &Request::get(self.repo_endpoint(&format!("actions/runs?run_number={number}"))),
            )
            .await?;
        run_id(&runs, number, head)
    }

    /// Web URLs use a repository run number and zero-based job index, neither
    /// API ID. A host may link over HTTPS while its remote uses HTTP.
    pub(crate) fn job_location(&self, target: &str) -> Result<(u64, usize)> {
        let relative = ["https", "http"]
            .iter()
            .find_map(|scheme| target.strip_prefix(&format!("{scheme}://{}", self.host)))
            .unwrap_or(target);
        let prefix = format!("/{}/actions/runs/", self.path);
        let parts = relative
            .strip_prefix(&prefix)
            .context("check does not link to this repository's Actions job")?
            .split('/')
            .collect::<Vec<_>>();
        let [run, "jobs", job] = parts.as_slice() else {
            anyhow::bail!("invalid Actions job URL")
        };
        let run = run.parse::<u64>().context("invalid Actions run number")?;
        let job = job.parse::<usize>().context("invalid Actions job index")?;
        ensure!(run > 0, "invalid Actions run number");
        Ok((run, job))
    }
}

fn run_id(runs: &Value, number: u64, head: &str) -> Result<u64> {
    let runs = runs["workflow_runs"]
        .as_array()
        .context("Actions runs are not a list")?;
    let [run] = runs.as_slice() else {
        anyhow::bail!("Actions run number did not identify one run")
    };
    ensure!(
        run["index_in_repo"] == number && run["commit_sha"] == head,
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

impl CheckLog {
    fn from_text(url: &str, name: &str, text: &str) -> Self {
        let failed_step = failed_step(text);
        let mut start = text.len().saturating_sub(TAIL_BYTES);
        while !text.is_char_boundary(start) {
            start += 1;
        }
        Self {
            url: url.into(),
            name: name.into(),
            text: text[start..].into(),
            truncated: start > 0,
            failed_step,
        }
    }
}

fn failed_step(text: &str) -> Option<String> {
    let mut current = None;
    for line in text.lines() {
        if let Some((_, step)) = line.split_once("⭐ Run ") {
            current = Some(step.trim().chars().take(256).collect());
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn web_job_locations_are_repository_bound_and_use_indices() {
        let forge = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
        let http = ForgeRepo::parse("http://forge.example/team/repo").unwrap();
        for target in [
            "/team/repo/actions/runs/2765/jobs/1",
            "https://forge.example/team/repo/actions/runs/2765/jobs/1",
            "http://forge.example/team/repo/actions/runs/2765/jobs/1",
        ] {
            assert_eq!(forge.job_location(target).unwrap(), (2765, 1));
            assert_eq!(http.job_location(target).unwrap(), (2765, 1));
        }
        for target in [
            "https://evil.example/team/repo/actions/runs/2765/jobs/1",
            "/other/repo/actions/runs/2765/jobs/1",
            "/team/repo/actions/runs/0/jobs/1",
            "/team/repo/actions/runs/2765/jobs/1/logs",
        ] {
            assert!(forge.job_location(target).is_err(), "{target}");
        }
        let runs =
            json!({"workflow_runs": [{"id": 9331, "index_in_repo": 2765, "commit_sha": "head"}]});
        assert_eq!(run_id(&runs, 2765, "head").unwrap(), 9331);
        assert!(run_id(&runs, 2765, "other").is_err());
        let jobs = json!([{"id": 18344, "run_id": 9331, "status": "success"}, {"id": 18345, "run_id": 9331, "status": "failure", "task_id": 9850}]);
        assert_eq!(job_id(&jobs, 1, 9331).unwrap(), 18345);
        assert!(job_id(&jobs, 0, 9331).is_err());
        assert!(job_id(&jobs, 1, 7).is_err());
    }

    #[test]
    fn tails_are_bounded_and_retain_step_before_long_output() {
        let text = format!(
            "⭐ Run Main Test\n{}\n⚙️ [runner]: exitcode '1': failure\nJob failed\n",
            "🦀".repeat(TAIL_BYTES)
        );
        let log = CheckLog::from_text("pr", "rust", &text);
        assert!(log.truncated);
        assert!(log.text.len() <= TAIL_BYTES);
        assert_eq!(log.failed_step.as_deref(), Some("Main Test"));
        assert!(log.text.ends_with("Job failed\n"));
        assert!(!CheckLog::from_text("pr", "rust", "short").truncated);
        let long = CheckLog::from_text("pr", "rust", &"\0".repeat(TAIL_BYTES * 2));
        assert_eq!(long.text.len(), TAIL_BYTES);
    }
}
