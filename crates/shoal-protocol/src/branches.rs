//! A repository's branches on its forge as `shoal branch ls` lists them,
//! with what decides whether one can go, and those `shoal branch delete`
//! deleted.
use serde::{Deserialize, Serialize};
use shoal_ids::space_ids::{SpaceId, SpaceNumber};
use shoal_refs::{BranchName, CommitId};
use shoal_states::states;

use crate::{
    forge::{branches::ForgeBranch, numbers::PullNumber},
    space_names::SpaceName,
};

/// A branch on the forge with what decides whether it can go.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListedBranch {
    #[serde(flatten)]
    pub branch: ForgeBranch,
    /// Whether it is the repository's default branch.
    pub default: bool,
    /// Whether the default branch has its changes, merged, squashed or
    /// rebased; none where Shoal cannot tell, such as in a repository that
    /// is not registered or for a commit the daemon has not fetched.
    pub merged: Option<bool>,
    /// The open PRs whose head it is.
    pub open_prs: Vec<PullNumber>,
    /// The space whose branch it is, if one is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space: Option<BranchSpace>,
}

/// The space a branch on the forge belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchSpace {
    pub id: SpaceId,
    pub number: SpaceNumber,
    pub name: SpaceName,
    pub state: SpaceWork,
}

states!(
    /// Where a space's work stands, which decides who may delete its branch
    /// on the forge.
    SpaceWork: Variants {
        /// Neither marked done nor merged into the default branch.
        Active => "active",
        /// Marked done.
        Done => "done",
        /// Its changes are on the default branch.
        Merged => "merged",
    }
);

/// A branch `shoal branch delete` deleted on the forge, at the commit it
/// pointed to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletedBranch {
    pub name: BranchName,
    pub head: CommitId,
}
