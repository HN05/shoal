//! GitHub's workflow runs: starting, listing and reading them and their jobs,
//! and acting on them.
use anyhow::{Context, Result};
use serde_json::{Value, json};
use shoal_refs::CommitId;

use super::{GitHub, actions};
use crate::{
    ForgeRepo,
    account::Account,
    backends::Runs,
    ci::{
        CiAction, Location,
        approval::RUNS_PAGE,
        id,
        run::{NewRun, workflow_segment},
        run_action::{Run, RunOperation},
        run_view::{JOBS_PAGE, RunFilter, RunJob},
    },
    items::item::text,
    numbers::{JobId, RunId, RunNumber},
    transport::api::{HttpMethod, Request, encode},
};

impl Runs for GitHub {
    /// GitHub accepts the dispatch without reporting the run it starts.
    fn start_run(&self, repo: &ForgeRepo, run: &NewRun) -> Result<Request> {
        let workflow = run.workflow_file(self)?;
        Ok(Request::new(
            HttpMethod::Post,
            repo.repo_endpoint(&format!("actions/workflows/{workflow}/dispatches")),
            Some(json!({"ref": run.branch, "inputs": run.inputs})),
        ))
    }

    /// A run's URL holds its API ID.
    fn locate_run(&self, repo: &ForgeRepo, target: &str) -> Option<RunNumber> {
        id(repo.repository_path(target, "/actions/runs/")?.first()?).map(RunNumber::new)
    }

    /// A run's number is its API ID.
    fn run_path(&self, number: RunNumber) -> String {
        format!("actions/runs/{number}")
    }

    /// A run names its workflow by the file's path in the repository.
    fn run_item(&self, _repo: &ForgeRepo, value: &Value) -> Result<Run> {
        let path = text(&value["path"]);
        Ok(Run {
            id: RunId::new(value["id"].as_u64().context("Actions run has no ID")?),
            url: text(&value["html_url"]).context("Actions run has no URL")?,
            branch: text(&value["head_branch"]),
            workflow: path.map(|path| path.rsplit('/').next().unwrap_or_default().into()),
            commit: text(&value["head_sha"]).map(CommitId::from),
            state: actions::state(value)?,
            succeeded: value["conclusion"] == "success",
            waiting: value["conclusion"] == "action_required",
        })
    }

    fn runs_path(&self, filter: &RunFilter) -> Result<String> {
        let mut path = match &filter.workflow {
            Some(workflow) => format!("actions/workflows/{}/runs", workflow_segment(workflow)?),
            None => "actions/runs".into(),
        };
        path.push_str(&format!("?per_page={}", filter.limit));
        if let Some(branch) = &filter.branch {
            path.push_str(&format!("&branch={}", encode(branch, false)));
        }
        if let Some(commit) = &filter.commit {
            path.push_str(&format!("&head_sha={}", encode(commit.as_str(), false)));
        }
        Ok(path)
    }

    /// GitHub filters the runs that wait for approval itself.
    fn waiting_runs_path(&self, head: &str, page: u32) -> Option<String> {
        Some(format!(
            "actions/runs?head_sha={}&status=action_required&per_page={RUNS_PAGE}&page={page}",
            encode(head, false)
        ))
    }

    fn run_jobs_path(&self, run: &Run, page: u32) -> Option<String> {
        Some(format!(
            "actions/runs/{}/jobs?per_page={JOBS_PAGE}&page={page}",
            run.id
        ))
    }

    fn run_jobs(&self, _run: &Run, _offset: usize, value: &Value) -> Result<Vec<RunJob>> {
        let jobs = value["jobs"]
            .as_array()
            .context("Actions jobs are not a list")?;
        jobs.iter()
            .map(|job| {
                Ok(RunJob {
                    id: JobId::new(job["id"].as_u64().context("Actions job has no ID")?),
                    name: text(&job["name"]).context("Actions job has no name")?,
                    state: actions::state(job)?,
                    url: text(&job["html_url"]),
                })
            })
            .collect()
    }

    async fn run_job_log(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        run: &Run,
        job: &RunJob,
    ) -> Result<String> {
        let location = Location::GitHub {
            run: run.id,
            job: job.id,
        };
        repo.job_log(account, location).await
    }

    fn run_operation(&self, run: &Run, action: CiAction, failed: bool) -> Result<RunOperation> {
        let id = run.id;
        Ok(RunOperation::Post(match (action, failed) {
            (CiAction::Rerun, false) => format!("actions/runs/{id}/rerun"),
            (CiAction::Rerun, true) => format!("actions/runs/{id}/rerun-failed-jobs"),
            (CiAction::Cancel, _) => format!("actions/runs/{id}/cancel"),
        }))
    }

    fn approve_endpoint(&self, run: &Run) -> Result<String> {
        Ok(format!("actions/runs/{}/approve", run.id))
    }
}
