use serde::{Deserialize, Serialize};

use super::Lifetime;
use crate::reviews::{FindingId, Outcome, ReviewTarget};

/// Requests about local reviews and the answers to their findings.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewMethod {
    /// Queue a review of the workspace, or return the one already queued.
    RunReview {
        workspace: String,
        target: ReviewTarget,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<crate::agent::Agent>,
    },
    /// Cancel review `id`, else the workspace's queued or running one.
    CancelReview {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<i64>,
    },
    /// Reviews of the workspaces, newest first: of one workspace, of a
    /// repository's, or of all.
    ListReviews {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repository: Option<String>,
    },
    /// Review `id`, else the workspace's newest.
    ShowReview {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<i64>,
    },
    /// Long-lived: review `id`, else the workspace's queued or running one,
    /// once it ends.
    AwaitReview {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<i64>,
    },
    ReplyToFinding {
        workspace: String,
        finding: FindingId,
        body: String,
    },
    /// Resolve a finding with `outcome`, or reopen it without one.
    ResolveFinding {
        workspace: String,
        finding: FindingId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outcome: Option<Outcome>,
    },
}

impl ReviewMethod {
    pub fn lifetime(&self) -> Lifetime {
        match self {
            Self::AwaitReview { .. } => Lifetime::Resumed,
            _ => Lifetime::Reply,
        }
    }
}
