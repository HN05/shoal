use serde::{Deserialize, Serialize};

/// Requests about CI checks and workflow runs of the repository of a
/// workspace or registered checkout, or of another repository a run's or
/// workflow's URL names.
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
    /// number in it; `failed` reruns only its failed jobs. A run of another
    /// repository than the context's, or without one, is named by its URL.
    CiRunAction {
        #[serde(flatten)]
        context: Option<super::ItemContext>,
        run: String,
        action: crate::forge::ci::CiAction,
        failed: bool,
    },
    /// One CI workflow run.
    ViewCiRun {
        #[serde(flatten)]
        context: Option<super::ItemContext>,
        run: String,
    },
    /// A CI workflow run's jobs, sent in pages.
    CiRunJobs {
        #[serde(flatten)]
        context: Option<super::ItemContext>,
        run: String,
    },
    /// The log of one job of a CI workflow run.
    CiRunJobLog {
        #[serde(flatten)]
        context: Option<super::ItemContext>,
        run: String,
        job: crate::forge::ci::run_view::JobRef,
    },
    /// The newest CI workflow runs, on the head commit of the PR `pr`
    /// names, otherwise by default on the workspace's branch or a checkout
    /// repository's default branch, sent in pages.
    ListCiRuns {
        #[serde(flatten)]
        context: super::ItemContext,
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
    /// Start a CI workflow run on a branch, by default the one a workflow
    /// file's URL names, the workspace's, or else the repository's default
    /// branch; a workflow URL needs no context.
    StartCiRun {
        #[serde(flatten)]
        context: Option<super::ItemContext>,
        workflow: Option<String>,
        branch: Option<String>,
        inputs: std::collections::BTreeMap<String, String>,
    },
}
