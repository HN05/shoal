//! A repository's branches as its forge lists them, and the open PRs that
//! join them.
use serde::{Deserialize, Serialize};
use shoal_refs::{BranchName, CommitId};

use crate::numbers::PullNumber;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForgeBranch {
    pub name: BranchName,
    pub head: CommitId,
    /// When that commit was made, as the forge writes the time; none where
    /// the forge leaves it out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub committed_at: Option<String>,
    /// Whether the forge keeps it from being deleted or pushed to freely.
    pub protected: bool,
}

/// An open PR of a repository, or from a branch of it into the repository
/// it is a fork of, by the branches it joins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenPull {
    pub number: PullNumber,
    /// Its head branch, of this repository unless `fork`.
    pub head: BranchName,
    /// Whether its head branch is a fork's, so no branch of this
    /// repository is its head.
    pub fork: bool,
    /// The branch of this repository it merges into; none for a PR into the
    /// repository this one is a fork of.
    pub base: Option<BranchName>,
}

impl OpenPull {
    /// Whether `branch` of this repository is its head or base, so that
    /// deleting the branch would close the PR or leave it without a base.
    pub fn joins(&self, branch: &BranchName) -> bool {
        (!self.fork && &self.head == branch) || self.base.as_ref() == Some(branch)
    }
}
