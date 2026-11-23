//! A PR's state, CI checks and reviews.

use serde::{Deserialize, Serialize};
use shoal_refs::CommitId;

use crate::{
    ci::{approval::WaitingRun, check_state::CheckState},
    items::item::ItemState,
};

shoal_states::states!(PrState {
    Open => "open",
    Merged => "merged",
    Closed => "closed",
});

impl From<ItemState> for PrState {
    fn from(state: ItemState) -> Self {
        match state {
            ItemState::Open => Self::Open,
            ItemState::Merged => Self::Merged,
            ItemState::Closed => Self::Closed,
        }
    }
}

shoal_states::states!(ReviewState {
    Unreviewed => "none",
    Commented => "commented",
    Approved => "approved",
    ChangesRequested => "changes_requested",
});

/// A failed lookup leaves the fields it would have filled empty and records
/// its error instead of failing the request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrStatus {
    pub url: crate::items::reference::ItemRef,
    pub state: Option<PrState>,
    /// Unknown while the forge computes mergeability, and for drafts and closed PRs.
    pub merge_conflicts: Option<bool>,
    pub checks: Vec<CheckResult>,
    /// The runs on the open PR's head that wait for approval, which report
    /// no checks until they are approved.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waiting_runs: Vec<WaitingRun>,
    pub review: Option<ReviewState>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
    /// The open PR's head commit, where its runs are looked up.
    #[serde(skip)]
    pub head: Option<CommitId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckResult {
    pub name: String,
    #[serde(deserialize_with = "crate::ci::check_state::saved")]
    pub result: CheckState,
}

impl PrStatus {
    pub fn new(url: crate::items::reference::ItemRef, state: Option<PrState>) -> Self {
        Self {
            url,
            state,
            merge_conflicts: None,
            checks: Vec::new(),
            waiting_runs: Vec::new(),
            review: None,
            errors: Vec::new(),
            head: None,
        }
    }

    pub fn failed(url: crate::items::reference::ItemRef, error: &anyhow::Error) -> Self {
        Self {
            errors: vec![format!("{error:#}")],
            ..Self::new(url, None)
        }
    }
}
