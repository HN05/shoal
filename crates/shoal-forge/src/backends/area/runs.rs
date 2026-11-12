//! What each forge does its own way with workflow runs: starting, listing and
//! reading them and their jobs, and acting on them.
use anyhow::Result;
use serde_json::Value;

use crate::{
    ForgeRepo,
    account::Account,
    backends::{Backend, ForgeKind},
    capability::Capability,
    ci::{
        CiAction,
        run::NewRun,
        run_action::{Run, RunOperation},
        run_view::{RunFilter, RunJob},
    },
    numbers::RunNumber,
    transport::api::Request,
};

/// How the forge starts, lists and reads workflow runs and their jobs,
/// and reruns, cancels and approves them.
#[shoal_forge_derive::forwarded(ForgeKind, dispatch)]
pub(crate) trait Runs: Backend {
    /// The request that starts `run`, or why the forge cannot start it.
    fn start_run(&self, repo: &ForgeRepo, run: &NewRun) -> Result<Request>;

    /// The web URL of the run a start request's `response` reports, if any.
    fn started_run_url(&self, _repo: &ForgeRepo, _response: &Value) -> Option<String> {
        None
    }

    /// The number a workflow run's web URL in the repository names, if
    /// `target` links one.
    fn locate_run(&self, repo: &ForgeRepo, target: &str) -> Option<RunNumber>;

    /// The path under the repository that reads the run numbered `number`
    /// in its web URL.
    fn run_path(&self, number: RunNumber) -> String;

    /// What precedes a run's number in the forge's run web URLs, or `None`
    /// where Shoal reads no runs.
    fn run_route(&self) -> Option<&'static str> {
        Some("/actions/runs/")
    }

    /// The run numbered `number` that a read of its path returned as `value`.
    fn run(&self, repo: &ForgeRepo, _number: RunNumber, value: &Value) -> Result<Run> {
        self.run_item(repo, value)
    }

    /// One run as the forge reads or lists it.
    fn run_item(&self, repo: &ForgeRepo, value: &Value) -> Result<Run>;

    /// The path under the repository that lists the runs `filter` selects,
    /// newest first.
    fn runs_path(&self, filter: &RunFilter) -> Result<String>;

    /// The path under the repository that lists page `page` of the runs on
    /// the commit `head` that may wait for approval,
    /// [`crate::ci::approval::RUNS_PAGE`] to a page, or `None` where the forge
    /// holds no runs for it.
    fn waiting_runs_path(&self, _head: &str, _page: u32) -> Option<String> {
        None
    }

    /// The runs a read of the list path returned.
    fn listed_runs<'a>(&self, value: &'a Value) -> Option<&'a [Value]> {
        value["workflow_runs"].as_array().map(Vec::as_slice)
    }

    /// The path under the repository that lists page `page` of `run`'s
    /// jobs, [`crate::ci::run_view::JOBS_PAGE`] to a page, or `None` past the
    /// last page the forge
    /// can return.
    fn run_jobs_path(&self, run: &Run, page: u32) -> Option<String>;

    /// The jobs of `run` that a read of a page of its jobs returned as
    /// `value`, after `offset` jobs on earlier pages.
    fn run_jobs(&self, run: &Run, offset: usize, value: &Value) -> Result<Vec<RunJob>>;

    /// The log of `run`'s `job` as plain text.
    async fn run_job_log(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        run: &Run,
        job: &RunJob,
    ) -> Result<String>;

    /// The requests that apply `action` to `run`; `failed` reruns only its
    /// failed jobs.
    fn run_operation(&self, run: &Run, action: CiAction, failed: bool) -> Result<RunOperation>;

    /// The endpoint under the repository that approves `run`, where the
    /// forge approves runs.
    fn approve_endpoint(&self, _run: &Run) -> Result<String> {
        Err(crate::capability::unsupported(
            self,
            Capability::ApproveRuns,
        ))
    }
}
