//! Absorbing a space into one above it: what `shoal absorb` asks and what
//! the absorbed space's work became.
use serde::{Deserialize, Serialize};
use shoal_refs::{BranchName, CommitId};

use crate::{
    forge::items::action::MergeMethod,
    item_numbers::ItemNumber,
    model::{SpaceLinks, SpaceRef},
    space_names::SpaceSelector,
};

/// Fold the space `space` into the space `into`, which takes its work,
/// links, files, threads and children, then remove it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Absorb {
    pub space: SpaceSelector,
    /// The space that takes the work: the caller's own by default, or for
    /// the user the absorbed space's parent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub into: Option<SpaceSelector>,
    /// How its branch merges, else by `[merge] method`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<MergeMethod>,
    /// A squash's commit message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Leave its uncommitted files behind instead of carrying them over.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub discard_uncommitted: bool,
    /// Absorb it as it is though it is not done, stopping its agent, or
    /// discard its code where the space that takes it has no branch for it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub force: bool,
}

/// What absorbing a space did; its removal goes on in the background.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Absorbed {
    /// The absorbed space.
    pub space: SpaceRef,
    /// The space that took its work.
    pub into: SpaceRef,
    /// Its branch's merge, when code merged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge: Option<AbsorbedCode>,
    /// The files carried over as uncommitted changes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub carried: Vec<String>,
    /// Whether its code was left behind, kept by its removal's checkpoint
    /// and branch.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub discarded: bool,
    /// The links that moved.
    pub links: SpaceLinks,
    /// The names of the attached files that moved.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    /// The open threads that moved, by their new numbers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub threads: Vec<ItemNumber>,
    /// The spaces directly below it, which moved up one level.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<SpaceRef>,
    /// Its pending PR proposals and push requests, withdrawn.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub withdrawn: Vec<ItemNumber>,
    /// What could not move, which the rest did without.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// An absorbed space's branch, merged into the branch of the space that
/// took it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AbsorbedCode {
    pub branch: BranchName,
    pub into: BranchName,
    pub method: MergeMethod,
    /// The commit the branch that took it is at.
    pub commit: CommitId,
    /// The merged commits, each as `<short hash> <subject>`, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commits: Vec<String>,
    /// False when that branch already had its commits.
    pub updated: bool,
}
