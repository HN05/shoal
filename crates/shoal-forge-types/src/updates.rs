//! What Shoal reports on watched issues, PRs and CI runs, and what each
//! snapshot of them observed.
mod state;

use serde::{Deserialize, Serialize};

pub use state::{SnapshotKind, SnapshotState};

shoal_states::states!(UpdateKind {
    Comment => "comment",
    CiCompleted => "ci_completed",
    MergeConflict => "merge_conflict",
    Closed => "closed",
    Merged => "merged",
    Reopened => "reopened",
    /// The title, description, labels or other details changed.
    Edited => "edited",
    LookupFailed => "lookup_failed",
    /// The base workspace's PRs merged; rebase onto their target branch.
    BaseMerged => "base_merged",
});

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Update {
    pub url: String,
    pub kind: UpdateKind,
    pub message: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub delivery: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<crate::activity::status::CheckResult>,
    /// The child workspace whose item this is, when its parent's agent
    /// receives the update.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

/// One line naming the item, the kind of change and the change, after the
/// child workspace it belongs to.
impl std::fmt::Display for Update {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(workspace) = &self.workspace {
            write!(f, "{workspace}: ")?;
        }
        write!(f, "{} [{}]: {}", self.url, self.kind, self.message)
    }
}
