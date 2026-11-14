//! A merged PR, with what reverting its merge with Git needs.

use serde::{Deserialize, Serialize};
use shoal_refs::CommitId;

use crate::items::item::Item;

/// A merged PR and what its merge left on the base branch, for reverting it
/// with Git.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergedPull {
    pub item: Item,
    /// The commit the merge left at the tip of the base branch: a merge
    /// commit, the squashed commit, or the last of the rebased ones.
    pub merge_commit: CommitId,
    /// The commit the PR's head branch pointed to when it merged.
    pub head: CommitId,
    /// The PR's own commits, which a rebase merge copied onto the base.
    pub commits: Vec<CommitId>,
    /// Where a remote of the repository keeps the PR's head, so its
    /// commits can be fetched after its branch goes.
    pub head_ref: Option<String>,
}
