//! What each forge supports. Each backend declares its capabilities, and a
//! request that needs one the forge lacks is refused before anything is sent,
//! with one error that names the forge and what it lacks.
use anyhow::Result;
use serde::Serialize;

use crate::state::states;
use crate::{ForgeRepo, backends::Backend, ci::CiAction, items::action::MergeMethod};

states!(Capability: Variants {
    ResolveThreads => "resolve_threads",
    RerunJobs => "rerun_jobs",
    CancelJobs => "cancel_jobs",
    StartRuns => "start_runs",
    ReadRuns => "read_runs",
    ReadJobLogs => "read_job_logs",
    ReopenPrs => "reopen_prs",
    Labels => "labels",
    DraftPrs => "draft_prs",
    Attachments => "attachments",
    MergeCommit => "merge_commit",
    RebaseMerge => "rebase_merge",
    SquashMerge => "squash_merge",
    ForkPrs => "fork_prs",
    Forks => "forks",
    LineComments => "line_comments",
    ApprovePrs => "approve_prs",
    RequestChanges => "request_changes",
    Reactions => "reactions",
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
            Self::ReopenPrs => "reopen PRs",
            Self::Labels => "label issues and PRs",
            Self::DraftPrs => "mark PRs as drafts",
            Self::Attachments => "attach files",
            Self::MergeCommit => "merge PRs with a merge commit",
            Self::RebaseMerge => "rebase and merge PRs",
            Self::SquashMerge => "squash and merge PRs",
            Self::ForkPrs => "open PRs from a fork",
            Self::Forks => "fork repositories",
            Self::LineComments => "comment on lines of a PR's diff",
            Self::ApprovePrs => "approve PRs",
            Self::RequestChanges => "request changes on PRs",
            Self::Reactions => "react to issues, PRs and comments",
        }
    }

    pub fn merge(method: MergeMethod) -> Self {
        match method {
            MergeMethod::Merge => Self::MergeCommit,
            MergeMethod::Rebase => Self::RebaseMerge,
            MergeMethod::Squash => Self::SquashMerge,
        }
    }

    pub fn ci(action: CiAction) -> Self {
        match action {
            CiAction::Rerun => Self::RerunJobs,
            CiAction::Cancel => Self::CancelJobs,
        }
    }
}

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Capabilities {
    pub forge: &'static str,
    pub capabilities: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Entry {
    pub capability: Capability,
    pub supported: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<&'static str>,
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

    pub fn supports(&self, capability: Capability) -> bool {
        self.kind.support(capability) == Support::Supported
    }

    pub fn capabilities(&self) -> Capabilities {
        Capabilities {
            forge: self.kind.name(),
            capabilities: Capability::ALL
                .iter()
                .map(|&capability| {
                    let (supported, hint) = match self.kind.support(capability) {
                        Support::Supported => (true, None),
                        Support::Unsupported(hint) => (false, hint),
                    };
                    Entry {
                        capability,
                        supported,
                        hint,
                    }
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests;
