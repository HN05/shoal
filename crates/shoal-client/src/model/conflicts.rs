//! A workspace's committed HEAD against a target: the diff base and
//! conflict checks.
use crate::space_ids::SpaceId;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize)]
pub struct DiffBase {
    pub workspace_id: SpaceId,
    pub commit: String,
    /// The worktree, verified as the workspace's.
    pub path: PathBuf,
}

/// Whether a workspace's committed HEAD merges cleanly into a target.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictCheck {
    pub workspace_id: SpaceId,
    pub target: String,
    pub target_commit: String,
    pub head: String,
    pub conflicts: bool,
    /// Conflicted paths, empty when the merge is clean.
    pub files: Vec<String>,
    /// The repository's sensitive paths that HEAD and the target both
    /// changed since their merge base, even where their changes merge.
    pub changed_on_both_sides: Vec<String>,
}

/// The sensitive paths that a workspace's HEAD and its default conflict
/// target both changed, as the last check found them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedOnBothSides {
    pub target: String,
    pub target_commit: String,
    pub head: String,
    /// Empty when the check found none.
    pub files: Vec<String>,
}

/// The sensitive files a merge brings to a target that changed them too,
/// as the last check of the merged head found them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeWarning {
    /// What is merged: a PR or a branch.
    pub subject: String,
    pub target: String,
    pub files: Vec<String>,
    /// The URL of the PR the warning is about, so the merge the user
    /// confirms is of that PR; none for a branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

impl std::fmt::Display for MergeWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (subject, target) = (&self.subject, &self.target);
        let files = self.files.join(", ");
        write!(
            f,
            "Sensitive files changed on both sides by {subject} and {target}: {files}"
        )
    }
}

impl ConflictCheck {
    /// Whether the merge neither conflicts nor changes a sensitive path on
    /// both sides.
    pub fn clean(&self) -> bool {
        !self.conflicts && self.changed_on_both_sides.is_empty()
    }
}
