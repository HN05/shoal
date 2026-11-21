use super::{Forwarding, Target};
use serde::{Deserialize, Serialize};

/// Requests about CI checks and workflow runs of the target repository: that
/// of a workspace or registered checkout, or another one `--repo` or a run's,
/// PR's or workflow's URL names.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CiMethod {
    /// The log of a failed check of a PR.
    CheckLog {
        #[serde(flatten)]
        target: super::RepoTarget,
        selection: crate::forge::items::link::Selection,
        check: String,
    },
    /// Rerun failed or cancel running checks on a PR's head; none named means all.
    CiAction {
        #[serde(flatten)]
        target: super::RepoTarget,
        item: Option<crate::forge::items::reference::ItemInput>,
        action: crate::forge::ci::CiAction,
        checks: Vec<String>,
    },
    /// Rerun or cancel one CI workflow run, named by its web URL or the
    /// number in it; `failed` reruns only its failed jobs.
    CiRunAction {
        #[serde(flatten)]
        target: super::RepoTarget,
        run: crate::forge::items::reference::ItemInput,
        action: crate::forge::ci::CiAction,
        failed: bool,
    },
    /// One CI workflow run.
    ViewCiRun {
        #[serde(flatten)]
        target: super::RepoTarget,
        run: crate::forge::items::reference::ItemInput,
    },
    /// A CI workflow run's jobs, sent in pages.
    CiRunJobs {
        #[serde(flatten)]
        target: super::RepoTarget,
        run: crate::forge::items::reference::ItemInput,
    },
    /// The log of one job of a CI workflow run.
    CiRunJobLog {
        #[serde(flatten)]
        target: super::RepoTarget,
        run: crate::forge::items::reference::ItemInput,
        job: crate::forge::ci::run_view::JobRef,
    },
    /// The newest CI workflow runs, on the head commit of the PR `pr`
    /// names, otherwise by default on the workspace's branch or else the
    /// repository's default branch, sent in pages.
    ListCiRuns {
        #[serde(flatten)]
        target: super::RepoTarget,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pr: Option<crate::forge::items::reference::ItemInput>,
        filter: crate::forge::ci::run_view::RunFilter,
    },
    /// Approve the run `run` names, or every run on the head of the PR `pr`
    /// names, by default the linked one, that waits for approval, as the
    /// user.
    ApproveCiRuns {
        #[serde(flatten)]
        target: super::RepoTarget,
        run: Option<crate::forge::items::reference::ItemInput>,
        pr: Option<crate::forge::items::reference::ItemInput>,
    },
    /// Start a CI workflow run on a branch, by default the one a workflow
    /// file's URL names, the workspace's, or else the repository's default
    /// branch.
    StartCiRun {
        #[serde(flatten)]
        target: super::RepoTarget,
        workflow: Option<String>,
        branch: Option<String>,
        inputs: std::collections::BTreeMap<String, String>,
    },
}

impl CiMethod {
    /// Each asks the forge of the space's repository.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::CheckLog { .. }
            | Self::CiAction { .. }
            | Self::CiRunAction { .. }
            | Self::ViewCiRun { .. }
            | Self::CiRunJobs { .. }
            | Self::CiRunJobLog { .. }
            | Self::ListCiRuns { .. }
            | Self::ApproveCiRuns { .. }
            | Self::StartCiRun { .. } => Forwarding::Online,
        }
    }

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::CheckLog { target, .. }
            | Self::CiAction { target, .. }
            | Self::CiRunAction { target, .. }
            | Self::ViewCiRun { target, .. }
            | Self::CiRunJobs { target, .. }
            | Self::CiRunJobLog { target, .. }
            | Self::ListCiRuns { target, .. }
            | Self::ApproveCiRuns { target, .. }
            | Self::StartCiRun { target, .. } => target.named_space().map(Target::Space),
        }
    }
}
