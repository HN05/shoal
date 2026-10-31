use serde::{Deserialize, Serialize};

use super::Lifetime;
use crate::reviews::{FindingId, Outcome, ReviewTarget};

/// Requests about local reviews and the answers to their findings. Reviews
/// come back as pieces of their JSON, in as many frames as they need.
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
    /// repository's, or of all, in as many frames as they need.
    ListReviews {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repository: Option<crate::repository::Selector>,
    },
    /// Review `id`, else the workspace's newest.
    ShowReview {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<i64>,
    },
    /// Long-lived: the ID of review `id`, else of the workspace's latest,
    /// once it ends.
    AwaitReview {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<i64>,
    },
    /// `workspace` is the finding's, which scope checks fill in.
    ReplyToFinding {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        finding: FindingId,
        body: String,
    },
    /// Refuse `shoal pr open` of a new PR before it pushes when the
    /// workspace's review gate would; only workspace processes are gated.
    CheckPrGate { workspace: String },
    /// Resolve a finding with `outcome`, or reopen it without one.
    ResolveFinding {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
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

/// A piece of the JSON of a review response, which the client joins.
#[derive(Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReviewPiece(pub String);
