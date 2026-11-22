//! Bitbucket's workflow runs: starting, listing and reading them and their
//! jobs, and acting on them.
use anyhow::Result;
use serde_json::{Value, json};

use super::Bitbucket;
use crate::{
    backends::Runs,
    capability,
    ci::{run::NewRun, run_action::RunOperation},
    transport::{
        account::Account,
        api::{Request, TransportRequests},
    },
};
use shoal_forge_types::{
    ForgeRepo,
    capability::Capability,
    ci::{
        CiAction,
        run_action::Run,
        run_view::{RunFilter, RunJob},
    },
    numbers::{JobId, RunNumber},
    raw::HttpMethod,
};

impl Runs for Bitbucket {
    /// A workflow names a custom pipeline; without one the branch's own
    /// pipeline runs. Inputs are its variables.
    fn start_run(&self, repo: &ForgeRepo, run: &NewRun) -> Result<Request> {
        let mut target = json!({
            "type": "pipeline_ref_target",
            "ref_type": "branch",
            "ref_name": run.branch,
        });
        if let Some(pipeline) = &run.workflow {
            target["selector"] = json!({"type": "custom", "pattern": pipeline});
        }
        let variables = run
            .inputs
            .iter()
            .map(|(key, value)| json!({"key": key, "value": value}))
            .collect::<Vec<_>>();
        Ok(Request::new(
            HttpMethod::Post,
            repo.repo_endpoint("pipelines"),
            Some(json!({"target": target, "variables": variables})),
        ))
    }

    fn started_run_url(&self, repo: &ForgeRepo, response: &Value) -> Option<String> {
        let number = response["build_number"].as_u64()?;
        Some(format!(
            "{}/pipelines/results/{number}",
            repo.repository_url()
        ))
    }

    fn run_path(&self, number: RunNumber) -> String {
        format!("pipelines/{number}")
    }

    /// Shoal reads no Pipelines run, which the capabilities refuse first.
    fn run_item(&self, _repo: &ForgeRepo, _value: &Value) -> Result<Run> {
        Err(capability::unsupported(self, Capability::ReadRuns))
    }

    fn runs_path(&self, _filter: &RunFilter) -> Result<String> {
        Err(capability::unsupported(self, Capability::ReadRuns))
    }

    fn run_jobs_path(&self, _run: &Run, _page: u32) -> Option<String> {
        None
    }

    fn run_jobs(&self, _run: &Run, _offset: usize, _value: &Value) -> Result<Vec<RunJob>> {
        Err(capability::unsupported(self, Capability::ReadRuns))
    }

    async fn run_job_log(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _run: &Run,
        _job: &RunJob,
    ) -> Result<String> {
        Err(capability::unsupported(self, Capability::ReadJobLogs))
    }

    fn rerun_job_endpoint(&self, _job: JobId) -> Result<String> {
        Err(capability::unsupported(self, CiAction::Rerun.capability()))
    }

    fn run_operation(&self, _run: &Run, action: CiAction, _failed: bool) -> Result<RunOperation> {
        Err(capability::unsupported(self, action.capability()))
    }
}
