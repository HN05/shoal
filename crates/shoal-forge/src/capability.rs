//! What each forge supports. Each backend declares its capabilities, and a
//! request that needs one the forge lacks is refused before anything is sent,
//! with one error that names the forge and what it lacks.
use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::state::states;
use crate::{
    ForgeRepo,
    account::Account,
    backends::Backend,
    ci::CiAction,
    items::{
        action::{MergeMethod, UpdateMethod},
        link::ItemKind,
    },
};

states!(Capability: Variants {
    ResolveThreads => "resolve_threads",
    RerunJobs => "rerun_jobs",
    CancelJobs => "cancel_jobs",
    StartRuns => "start_runs",
    ReadRuns => "read_runs",
    ReadJobLogs => "read_job_logs",
    ApproveRuns => "approve_runs",
    ReopenPrs => "reopen_prs",
    Labels => "labels",
    IssueAssignees => "issue_assignees",
    PrAssignees => "pr_assignees",
    IssueMilestones => "issue_milestones",
    PrMilestones => "pr_milestones",
    DraftPrs => "draft_prs",
    Attachments => "attachments",
    Downloads => "downloads",
    MergeCommit => "merge_commit",
    RebaseMerge => "rebase_merge",
    SquashMerge => "squash_merge",
    AutoMerge => "auto_merge",
    UpdateByMerge => "update_by_merge",
    UpdateByRebase => "update_by_rebase",
    ForkPrs => "fork_prs",
    Forks => "forks",
    LineComments => "line_comments",
    ApprovePrs => "approve_prs",
    RequestChanges => "request_changes",
    Reactions => "reactions",
    RevertPrs => "revert_prs",
    RunJobs => "run_jobs",
});

impl Capability {
    /// What the capability lets Shoal do, as a verb phrase.
    fn action(self) -> &'static str {
        match self {
            Self::ResolveThreads => "resolve review threads",
            Self::RerunJobs => "rerun CI jobs",
            Self::CancelJobs => "cancel CI jobs",
            Self::StartRuns => "start CI workflow runs",
            Self::ReadRuns => "read CI workflow runs",
            Self::ReadJobLogs => "read CI job logs",
            Self::ApproveRuns => "approve CI runs",
            Self::ReopenPrs => "reopen PRs",
            Self::Labels => "label issues and PRs",
            Self::IssueAssignees => "assign issues",
            Self::PrAssignees => "assign PRs",
            Self::IssueMilestones => "set the milestone of issues",
            Self::PrMilestones => "set the milestone of PRs",
            Self::DraftPrs => "mark PRs as drafts",
            Self::Attachments => "attach files",
            Self::Downloads => "download images and attached files",
            Self::MergeCommit => "merge PRs with a merge commit",
            Self::RebaseMerge => "rebase and merge PRs",
            Self::SquashMerge => "squash and merge PRs",
            Self::AutoMerge => "merge PRs once their checks pass",
            Self::UpdateByMerge => "update PR branches by merging their base",
            Self::UpdateByRebase => "update PR branches by rebasing onto their base",
            Self::ForkPrs => "open PRs from a fork",
            Self::Forks => "fork repositories",
            Self::LineComments => "comment on lines of a PR's diff",
            Self::ApprovePrs => "approve PRs",
            Self::RequestChanges => "request changes on PRs",
            Self::Reactions => "react to issues, PRs and comments",
            Self::RevertPrs => "have the forge revert merged PRs",
            Self::RunJobs => "run CI jobs on this machine",
        }
    }

    pub fn merge(method: MergeMethod) -> Self {
        match method {
            MergeMethod::Merge => Self::MergeCommit,
            MergeMethod::Rebase => Self::RebaseMerge,
            MergeMethod::Squash => Self::SquashMerge,
        }
    }

    pub fn update(method: UpdateMethod) -> Self {
        match method {
            UpdateMethod::Merge => Self::UpdateByMerge,
            UpdateMethod::Rebase => Self::UpdateByRebase,
        }
    }

    pub fn assignees(kind: ItemKind) -> Self {
        match kind {
            ItemKind::Issue => Self::IssueAssignees,
            ItemKind::Pr => Self::PrAssignees,
        }
    }

    pub fn milestones(kind: ItemKind) -> Self {
        match kind {
            ItemKind::Issue => Self::IssueMilestones,
            ItemKind::Pr => Self::PrMilestones,
        }
    }

    pub fn ci(action: CiAction) -> Self {
        match action {
            CiAction::Rerun => Self::RerunJobs,
            CiAction::Cancel => Self::CancelJobs,
        }
    }
}

/// What `shoal pr revert` does on a forge that cannot revert PRs itself.
pub(crate) const GIT_REVERT: &str =
    "shoal pr revert opens the revert from a new workspace with git revert";

/// Whether a forge has a capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Support {
    Supported,
    /// With what to do instead, or why, when the forge's way differs.
    Unsupported(Option<&'static str>),
}

/// A request that needs a capability the forge lacks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsupported {
    pub forge: &'static str,
    pub capability: Capability,
    pub hint: Option<&'static str>,
}

impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Shoal cannot {} on {}",
            self.capability.action(),
            self.forge
        )?;
        match self.hint {
            Some(hint) => write!(f, "; {hint}"),
            None => Ok(()),
        }
    }
}

impl std::error::Error for Unsupported {}

/// Refuse `capability` unless `backend` has it.
pub(crate) fn require(backend: &(impl Backend + ?Sized), capability: Capability) -> Result<()> {
    match backend.support(capability) {
        Support::Supported => Ok(()),
        Support::Unsupported(_) => Err(unsupported(backend, capability)),
    }
}

/// The refusal of `capability`, also for a backend method that cannot carry
/// out a request needing it.
pub(crate) fn unsupported(
    backend: &(impl Backend + ?Sized),
    capability: Capability,
) -> anyhow::Error {
    let hint = match backend.support(capability) {
        Support::Unsupported(hint) => hint,
        Support::Supported => None,
    };
    Unsupported {
        forge: backend.name(),
        capability,
        hint,
    }
    .into()
}

/// A repository's forge and whether it has each capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub forge: String,
    pub capabilities: Vec<Entry>,
}

impl Capabilities {
    /// Whether the forge has `capability`.
    pub fn supports(&self, capability: Capability) -> bool {
        self.capabilities
            .iter()
            .any(|entry| entry.capability == capability && entry.supported)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub capability: Capability,
    pub supported: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl ForgeRepo {
    /// Refuse a request that needs `capability` when the forge lacks it.
    pub fn require(&self, capability: Capability) -> Result<()> {
        require(&self.kind, capability)
    }

    /// Refuse a request that needs any of `capabilities` the forge lacks.
    pub fn require_all(&self, capabilities: impl IntoIterator<Item = Capability>) -> Result<()> {
        capabilities
            .into_iter()
            .try_for_each(|capability| self.require(capability))
    }

    /// Refuse a request that needs `capability` when the repository's
    /// server lacks it, asking the server where the forge's versions differ.
    pub async fn require_on_server(&self, account: &Account, capability: Capability) -> Result<()> {
        match self.kind.server_support(self, account, capability).await? {
            Support::Supported => Ok(()),
            Support::Unsupported(hint) => Err(Unsupported {
                forge: self.kind.name(),
                capability,
                hint,
            }
            .into()),
        }
    }

    pub fn supports(&self, capability: Capability) -> bool {
        self.kind.support(capability) == Support::Supported
    }

    pub fn capabilities(&self) -> Capabilities {
        self.listed(
            Capability::ALL
                .iter()
                .map(|&capability| (capability, self.kind.support(capability))),
        )
    }

    /// The capabilities of the repository's server, asking it where the
    /// forge's versions differ.
    pub async fn server_capabilities(&self, account: &Account) -> Result<Capabilities> {
        let mut supports = Vec::new();
        for &capability in Capability::ALL {
            let support = self.kind.server_support(self, account, capability).await?;
            supports.push((capability, support));
        }
        Ok(self.listed(supports))
    }

    fn listed(&self, supports: impl IntoIterator<Item = (Capability, Support)>) -> Capabilities {
        Capabilities {
            forge: self.kind.name().to_owned(),
            capabilities: supports
                .into_iter()
                .map(|(capability, support)| {
                    let (supported, hint) = match support {
                        Support::Supported => (true, None),
                        Support::Unsupported(hint) => (false, hint),
                    };
                    Entry {
                        capability,
                        supported,
                        hint: hint.map(str::to_owned),
                    }
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests;
