//! Reading CI workflow runs: one run with its jobs and their logs, or the
//! newest runs of a branch or workflow, as the workspace's agent account.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    ForgeRepo,
    account::Account,
    backends::Backend,
    capability::Capability,
    ci::{JobState, checks::CheckLog, run_action::Run},
    transport::api::Request,
};

/// The most runs one list returns.
pub const MAX_RUNS: usize = 100;

/// A job of a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunJob {
    /// The ID its API reads the log by.
    #[serde(skip)]
    pub(crate) id: u64,
    pub name: String,
    pub state: JobState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// A run with its jobs, and the job logs a reader added.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunView {
    #[serde(flatten)]
    pub run: Run,
    pub jobs: Vec<RunJob>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub logs: Vec<CheckLog>,
    /// The logs that could not be read, and why.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

/// The runs a list selects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunFilter {
    pub branch: Option<String>,
    pub workflow: Option<String>,
    pub limit: usize,
}

/// Refuse a filter no forge lists runs by, before any lookup.
pub fn validate_filter(filter: &RunFilter) -> Result<()> {
    ensure!(
        (1..=MAX_RUNS).contains(&filter.limit),
        "limit must be between 1 and {MAX_RUNS}"
    );
    for (field, value) in [("branch", &filter.branch), ("workflow", &filter.workflow)] {
        ensure!(
            value.as_ref().is_none_or(|value| !value.trim().is_empty()),
            "{field} must be nonempty"
        );
    }
    Ok(())
}

impl ForgeRepo {
    /// The run `input`, its web URL or the number in it, names, with its jobs.
    pub async fn view_run(&self, account: &Account, input: &str) -> Result<RunView> {
        self.require(Capability::ReadRuns)?;
        let run = self.read_run(account, input).await?;
        let read = Request::get(self.repo_endpoint(&self.kind.run_jobs_path(&run)));
        let jobs = self
            .kind
            .run_jobs(&run, &self.send(account, &read).await?)?;
        Ok(RunView {
            run,
            jobs,
            logs: Vec::new(),
            errors: Vec::new(),
        })
    }

    /// The log of the job `name` of the run `input` names.
    pub async fn run_job_log(
        &self,
        account: &Account,
        input: &str,
        name: &str,
    ) -> Result<CheckLog> {
        self.require_all([Capability::ReadRuns, Capability::ReadJobLogs])?;
        let view = self.view_run(account, input).await?;
        let job = view
            .jobs
            .iter()
            .find(|job| job.name == name)
            .with_context(|| format!("run {} has no job {name}", view.run.url))?;
        let log = self.kind.run_job_log(self, account, &view.run, job).await?;
        let log = CheckLog::from_text(&view.run.url, name, &log);
        Ok(match &job.url {
            Some(url) => log.with_job(url.clone()),
            None => log,
        })
    }

    /// The newest runs `filter` selects, newest first.
    pub async fn list_runs(&self, account: &Account, filter: &RunFilter) -> Result<Vec<Run>> {
        self.require(Capability::ReadRuns)?;
        validate_filter(filter)?;
        let read = Request::get(self.repo_endpoint(&self.kind.runs_path(filter)?));
        let value = self.send(account, &read).await?;
        let runs = self
            .kind
            .listed_runs(&value)
            .context("CI runs are not a list")?;
        runs.iter()
            .take(filter.limit)
            .map(|run| self.kind.run_item(self, run))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn filter(branch: Option<&str>, workflow: Option<&str>) -> RunFilter {
        RunFilter {
            branch: branch.map(Into::into),
            workflow: workflow.map(Into::into),
            limit: 10,
        }
    }

    fn path(remote: &str, filter: &RunFilter) -> Result<String> {
        ForgeRepo::parse(remote)?.kind.runs_path(filter)
    }

    #[test]
    fn each_forge_lists_a_branch_or_workflow_its_own_way() -> Result<()> {
        let github = "https://github.com/team/repo";
        assert_eq!(
            path(github, &filter(None, None))?,
            "actions/runs?per_page=10"
        );
        assert_eq!(
            path(github, &filter(Some("fix/a b"), Some("ci.yml")))?,
            "actions/workflows/ci.yml/runs?per_page=10&branch=fix%2Fa%20b"
        );
        let gitlab = "https://gitlab.com/team/repo";
        assert_eq!(
            path(gitlab, &filter(Some("topic"), None))?,
            "pipelines?per_page=10&ref=topic"
        );
        let error = path(gitlab, &filter(None, Some("ci.yml"))).unwrap_err();
        assert!(
            error.to_string().contains("leave out the workflow"),
            "{error}"
        );
        let forgejo = "https://codeberg.org/team/repo";
        assert_eq!(
            path(forgejo, &filter(Some("topic"), Some("deps.yml")))?,
            "actions/runs?limit=10&page=1&ref=refs%2Fheads%2Ftopic&workflow_id=deps.yml"
        );
        Ok(())
    }

    #[test]
    fn listed_runs_are_read_like_one_run() -> Result<()> {
        let repo = ForgeRepo::parse("https://gitlab.com/team/repo")?;
        let value = json!([{"id": 12, "web_url": "https://gitlab.com/team/repo/-/pipelines/12",
            "ref": "topic", "tag": false, "status": "failed"}]);
        let listed = repo.kind.listed_runs(&value).unwrap_or_default();
        let run = repo.kind.run_item(&repo, &listed[0])?;
        assert_eq!(
            (run.branch.as_deref(), run.state),
            (Some("topic"), JobState::Failed)
        );
        Ok(())
    }

    #[test]
    fn jobs_are_read_with_their_state_and_page() -> Result<()> {
        let github = ForgeRepo::parse("https://github.com/team/repo")?;
        let run = Run {
            id: 12,
            url: "https://github.com/team/repo/actions/runs/12".into(),
            branch: None,
            workflow: None,
            state: JobState::Failed,
        };
        let jobs = github.kind.run_jobs(
            &run,
            &json!({"jobs": [
                {"id": 34, "name": "rust", "status": "completed", "conclusion": "failure",
                    "html_url": "https://github.com/team/repo/actions/runs/12/job/34"},
                {"id": 35, "name": "docs", "status": "in_progress", "conclusion": null}
            ]}),
        )?;
        assert_eq!(
            jobs,
            [
                RunJob {
                    id: 34,
                    name: "rust".into(),
                    state: JobState::Failed,
                    url: Some("https://github.com/team/repo/actions/runs/12/job/34".into()),
                },
                RunJob {
                    id: 35,
                    name: "docs".into(),
                    state: JobState::Running,
                    url: None,
                },
            ]
        );
        // Forgejo links a job by its index in the run.
        let forgejo = ForgeRepo::parse("https://codeberg.org/team/repo")?;
        let run = Run {
            id: 9331,
            url: "https://codeberg.org/team/repo/actions/runs/2765".into(),
            ..run
        };
        let jobs = forgejo.kind.run_jobs(
            &run,
            &json!([{"id": 1, "name": "audit", "status": "success", "run_id": 9331},
                {"id": 2, "name": "report", "status": "failure", "run_id": 9331}]),
        )?;
        assert_eq!(
            jobs[1].url.as_deref(),
            Some("https://codeberg.org/team/repo/actions/runs/2765/jobs/1")
        );
        assert_eq!(jobs[1].state, JobState::Failed);
        Ok(())
    }

    #[test]
    fn filters_need_a_bounded_limit_and_nonempty_names() {
        assert!(validate_filter(&filter(Some("topic"), Some("ci.yml"))).is_ok());
        assert!(validate_filter(&filter(Some(" "), None)).is_err());
        let mut unbounded = filter(None, None);
        unbounded.limit = MAX_RUNS + 1;
        assert!(validate_filter(&unbounded).is_err());
    }
}
