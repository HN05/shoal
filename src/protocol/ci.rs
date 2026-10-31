use serde::{Deserialize, Serialize};

/// Requests about CI checks and workflow runs of a workspace's repository,
/// or for those that act on them, of a registered checkout's.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CiMethod {
    CheckLog {
        workspace: String,
        selection: crate::forge::items::link::Selection,
        check: String,
    },
    /// Rerun failed or cancel running checks on a PR's head; none named means all.
    CiAction {
        #[serde(flatten)]
        context: super::ItemContext,
        item: Option<String>,
        action: crate::forge::ci::CiAction,
        checks: Vec<String>,
    },
    /// Rerun or cancel one CI workflow run, named by its web URL or the
    /// number in it; `failed` reruns only its failed jobs.
    CiRunAction {
        #[serde(flatten)]
        context: super::ItemContext,
        run: String,
        action: crate::forge::ci::CiAction,
        failed: bool,
    },
    /// One CI workflow run.
    ViewCiRun { workspace: String, run: String },
    /// A CI workflow run's jobs, sent in pages.
    CiRunJobs { workspace: String, run: String },
    /// The log of one job of a CI workflow run.
    CiRunJobLog {
        workspace: String,
        run: String,
        job: crate::forge::ci::run_view::JobRef,
    },
    /// The newest CI workflow runs, on the head commit of the PR `pr`
    /// names, otherwise on the workspace's branch by default, sent in pages.
    ListCiRuns {
        workspace: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pr: Option<String>,
        filter: crate::forge::ci::run_view::RunFilter,
    },
    /// Approve the run `run` names, or every run on the head of the PR `pr`
    /// names, by default the linked one, that waits for approval, as the
    /// user.
    ApproveCiRuns {
        #[serde(flatten)]
        context: super::ItemContext,
        run: Option<String>,
        pr: Option<String>,
    },
    /// Start a CI workflow run on a branch, by default the workspace's.
    StartCiRun {
        workspace: String,
        workflow: Option<String>,
        branch: Option<String>,
        inputs: std::collections::BTreeMap<String, String>,
    },
}
