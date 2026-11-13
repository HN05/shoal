//! Forgejo's workflow runs: starting, listing and reading them and their jobs,
//! and acting on them.
use anyhow::{Context, Result};
use serde_json::{Value, json};
use shoal_refs::CommitId;

use super::{Forgejo, actions};
use crate::{
    ForgeRepo,
    account::Account,
    backends::{Checks, Runs},
    ci::{
        CiAction,
        approval::RUNS_PAGE,
        run::NewRun,
        run_action::{Run, RunOperation},
        run_view::{RunFilter, RunJob},
    },
    items::item::text,
    numbers::{JobId, RunId, RunNumber},
    transport::api::{HttpMethod, Request, encode},
};

impl Runs for Forgejo {
    /// Forgejo reports the run it starts when asked to; Gitea ignores the
    /// request and reports none.
    fn start_run(&self, repo: &ForgeRepo, run: &NewRun) -> Result<Request> {
        let workflow = run.workflow_file(self)?;
        Ok(Request::new(
            HttpMethod::Post,
            repo.repo_endpoint(&format!("actions/workflows/{workflow}/dispatches")),
            Some(json!({"ref": run.branch, "inputs": run.inputs, "return_run_info": true})),
        ))
    }

    fn started_run_url(&self, repo: &ForgeRepo, response: &Value) -> Option<String> {
        let number = response["run_number"].as_u64()?;
        Some(format!("{}/actions/runs/{number}", repo.repository_url()))
    }

    fn locate_run(&self, repo: &ForgeRepo, target: &str) -> Option<RunNumber> {
        actions::run_number(repo, target)
    }

    /// Forgejo links a run by its number in the repository, not its API ID.
    fn run_path(&self, number: RunNumber) -> String {
        format!("actions/runs?run_number={number}")
    }

    fn run(&self, repo: &ForgeRepo, number: RunNumber, value: &Value) -> Result<Run> {
        self.run_item(repo, actions::numbered_run(value, number)?)
    }

    /// A run names its workflow by file name.
    fn run_item(&self, repo: &ForgeRepo, value: &Value) -> Result<Run> {
        let number = value["index_in_repo"]
            .as_u64()
            .context("Actions run has no number")?;
        Ok(Run {
            id: RunId::new(value["id"].as_u64().context("Actions run has no ID")?),
            url: format!("{}/actions/runs/{number}", repo.repository_url()),
            branch: text(&value["prettyref"]),
            workflow: text(&value["workflow_id"]),
            commit: text(&value["commit_sha"]).map(CommitId::from),
            state: actions::state(&value["status"])?,
            succeeded: value["status"] == "success",
            waiting: value["need_approval"] == true,
        })
    }

    /// The list reads a page only when it names one.
    fn runs_path(&self, filter: &RunFilter) -> Result<String> {
        let mut path = format!("actions/runs?limit={}&page=1", filter.limit);
        if let Some(branch) = &filter.branch {
            let reference = format!("refs/heads/{branch}");
            path.push_str(&format!("&ref={}", encode(&reference, false)));
        }
        if let Some(workflow) = &filter.workflow {
            path.push_str(&format!("&workflow_id={}", encode(workflow, false)));
        }
        if let Some(commit) = &filter.commit {
            path.push_str(&format!("&head_sha={}", encode(commit.as_str(), false)));
        }
        Ok(path)
    }

    /// Forgejo marks the runs that wait for approval among the commit's.
    fn waiting_runs_path(&self, head: &str, page: u32) -> Option<String> {
        Some(format!(
            "actions/runs?limit={RUNS_PAGE}&page={page}&head_sha={}",
            encode(head, false)
        ))
    }

    /// Forgejo lists every job of a run at once.
    fn run_jobs_path(&self, run: &Run, page: u32) -> Option<String> {
        (page == 1).then(|| format!("actions/runs/{}/jobs", run.id))
    }

    /// The web UI links a job by its index in the run.
    fn run_jobs(&self, run: &Run, offset: usize, value: &Value) -> Result<Vec<RunJob>> {
        let jobs = value.as_array().context("Actions jobs are not a list")?;
        jobs.iter()
            .enumerate()
            .map(|(index, job)| {
                let index = offset + index;
                Ok(RunJob {
                    id: JobId::new(job["id"].as_u64().context("Actions job has no ID")?),
                    name: text(&job["name"]).context("Actions job has no name")?,
                    state: actions::job_state(job)?,
                    url: Some(format!("{}/jobs/{index}", run.url)),
                })
            })
            .collect()
    }

    async fn run_job_log(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        _run: &Run,
        job: &RunJob,
    ) -> Result<String> {
        let endpoint = repo.repo_endpoint(&format!("actions/jobs/{}/logs", job.id));
        Ok(self.plain_log(&actions::job_log(repo, account, &endpoint).await?))
    }

    /// Forgejo reruns failed jobs one by one; one run accepts several job
    /// reruns in a row, also while it runs again.
    fn run_operation(&self, run: &Run, action: CiAction, failed: bool) -> Result<RunOperation> {
        let id = run.id;
        Ok(match (action, failed) {
            (CiAction::Rerun, false) => RunOperation::Post(format!("actions/runs/{id}/rerun")),
            (CiAction::Rerun, true) => {
                RunOperation::EachFailedJob(|job| format!("actions/jobs/{job}/rerun"))
            }
            (CiAction::Cancel, _) => RunOperation::Post(format!("actions/runs/{id}/cancel")),
        })
    }
}
