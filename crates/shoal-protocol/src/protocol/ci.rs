use super::{Forwarding, Target};
use serde::{Deserialize, Serialize};

/// Whose CI runs a list reads.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunsOf {
    /// The branch its filter names, by default the workspace's, a chat's
    /// linked PR's head or else the repository's default branch.
    #[default]
    Branch,
    /// The head commit of the PR this names.
    Pull(crate::forge::items::reference::ItemInput),
    /// Every branch.
    EveryBranch,
}

impl RunsOf {
    pub fn is_branch(&self) -> bool {
        *self == Self::Branch
    }
}

/// Requests about CI checks and workflow runs of the target repository.
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
    CiRunJobLog {
        #[serde(flatten)]
        target: super::RepoTarget,
        run: crate::forge::items::reference::ItemInput,
        job: crate::forge::ci::run_view::JobRef,
    },
    /// The newest CI workflow runs of what `of` names, sent in pages.
    ListCiRuns {
        #[serde(flatten)]
        target: super::RepoTarget,
        #[serde(default, skip_serializing_if = "RunsOf::is_branch")]
        of: RunsOf,
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
