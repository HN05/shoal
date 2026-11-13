//! GitLab's workflow runs: starting, listing and reading them and their jobs,
//! and acting on them.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use shoal_refs::CommitId;

use super::{GitLab, status_state};
use crate::{
    ForgeRepo,
    account::Account,
    backends::Runs,
    ci::{
        CiAction, Location, id,
        run::NewRun,
        run_action::{Run, RunOperation},
        run_view::{JOBS_PAGE, RunFilter, RunJob},
    },
    items::item::text,
    numbers::{JobId, RunId, RunNumber},
    transport::api::{HttpMethod, Request, encode},
};

impl Runs for GitLab {
    /// A branch has one pipeline, from its `.gitlab-ci.yml`, whose
    /// `spec:inputs` take the run's inputs.
    fn start_run(&self, repo: &ForgeRepo, run: &NewRun) -> Result<Request> {
        ensure!(
            run.workflow.is_none(),
            "GitLab runs the branch's pipeline; leave out the workflow"
        );
        let mut body = json!({"ref": run.branch});
        // Older GitLab refuses inputs, so a run without them sends none.
        if !run.inputs.is_empty() {
            body["inputs"] = json!(run.inputs);
        }
        Ok(Request::new(
            HttpMethod::Post,
            repo.repo_endpoint("pipeline"),
            Some(body),
        ))
    }

    fn started_run_url(&self, _repo: &ForgeRepo, response: &Value) -> Option<String> {
        response["web_url"].as_str().map(Into::into)
    }

    /// A pipeline's URL holds its API ID.
    fn locate_run(&self, repo: &ForgeRepo, target: &str) -> Option<RunNumber> {
        let parts = repo.repository_path(target, "/-/pipelines/")?;
        let [pipeline] = parts.as_slice() else {
            return None;
        };
        id(pipeline).map(RunNumber::new)
    }

    /// A pipeline's number is its API ID.
    fn run_path(&self, number: RunNumber) -> String {
        format!("pipelines/{number}")
    }

    fn run_route(&self) -> Option<&'static str> {
        Some("/-/pipelines/")
    }

    /// A tag's pipeline ran on no branch, and a branch has one pipeline,
    /// so none names a workflow.
    fn run_item(&self, _repo: &ForgeRepo, value: &Value) -> Result<Run> {
        Ok(Run {
            id: RunId::new(value["id"].as_u64().context("pipeline has no ID")?),
            url: text(&value["web_url"]).context("pipeline has no URL")?,
            branch: text(&value["ref"]).filter(|_| value["tag"] != true),
            workflow: None,
            commit: text(&value["sha"]).map(CommitId::from),
            state: status_state(&value["status"])?.into(),
            succeeded: value["status"] == "success",
            waiting: false,
        })
    }

    fn runs_path(&self, filter: &RunFilter) -> Result<String> {
        ensure!(
            filter.workflow.is_none(),
            "GitLab runs the branch's pipeline; leave out the workflow"
        );
        let mut path = format!("pipelines?per_page={}", filter.limit);
        if let Some(branch) = &filter.branch {
            path.push_str(&format!("&ref={}", encode(branch, false)));
        }
        if let Some(commit) = &filter.commit {
            path.push_str(&format!("&sha={}", encode(commit.as_str(), false)));
        }
        Ok(path)
    }

    fn listed_runs<'a>(&self, value: &'a Value) -> Option<&'a [Value]> {
        value.as_array().map(Vec::as_slice)
    }

    fn run_jobs_path(&self, run: &Run, page: u32) -> Option<String> {
        Some(format!(
            "pipelines/{}/jobs?per_page={JOBS_PAGE}&page={page}",
            run.id
        ))
    }

    fn run_jobs(&self, _run: &Run, _offset: usize, value: &Value) -> Result<Vec<RunJob>> {
        let jobs = value.as_array().context("pipeline jobs are not a list")?;
        jobs.iter()
            .map(|job| {
                Ok(RunJob {
                    id: JobId::new(job["id"].as_u64().context("GitLab job has no ID")?),
                    name: text(&job["name"]).context("GitLab job has no name")?,
                    state: status_state(&job["status"])?.into(),
                    url: text(&job["web_url"]),
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
        repo.job_log(account, Location::GitLab { job: job.id })
            .await
    }

    /// GitLab retries only a pipeline's failed and cancelled jobs.
    fn run_operation(&self, run: &Run, action: CiAction, _failed: bool) -> Result<RunOperation> {
        let id = run.id;
        Ok(RunOperation::Post(match action {
            CiAction::Rerun => format!("pipelines/{id}/retry"),
            CiAction::Cancel => format!("pipelines/{id}/cancel"),
        }))
    }
}
