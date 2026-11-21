use serde::{Deserialize, Serialize};

use super::{Forwarding, Lifetime, Target};
use crate::reviews::{ReviewId, ReviewTarget};
use crate::space_names::SpaceSelector;

/// Requests about local reviews, whose findings are threads. Reviews come
/// back as pieces of their JSON, in as many frames as they need.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewMethod {
    /// Queue a review of the workspace, or return the one already queued.
    RunReview {
        workspace: SpaceSelector,
        target: ReviewTarget,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<crate::agent::Agent>,
        /// The model and effort asked for the agent.
        #[serde(default, skip_serializing_if = "crate::agent::ModelChoice::is_empty")]
        model: crate::agent::ModelChoice,
    },
    /// Cancel review `id`, else the workspace's queued or running one.
    CancelReview {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<SpaceSelector>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<ReviewId>,
    },
    /// Reviews of the workspaces, newest first: of one workspace, of a
    /// repository's, or of all, in as many frames as they need.
    ListReviews {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<SpaceSelector>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repository: Option<crate::repository::Selector>,
    },
    /// Review `id`, else the workspace's newest.
    ShowReview {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<SpaceSelector>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<ReviewId>,
    },
    /// Long-lived: the ID of review `id`, else of the workspace's latest,
    /// once it ends.
    AwaitReview {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<SpaceSelector>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<ReviewId>,
    },
    /// Refuse `shoal pr open` of a new PR before it pushes when the
    /// workspace's review gate would hold the caller.
    CheckPrGate { workspace: SpaceSelector },
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

impl ReviewMethod {
    /// A review runs on its workspace's machine, which keeps its threads,
    /// and a wait's answer comes as that machine writes it.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::RunReview { .. }
            | Self::CancelReview { .. }
            | Self::ListReviews { .. }
            | Self::ShowReview { .. }
            | Self::CheckPrGate { .. } => Forwarding::Online,
            Self::AwaitReview { .. } => Forwarding::Streamed,
        }
    }

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::RunReview { workspace, .. } | Self::CheckPrGate { workspace } => {
                Some(Target::Space(workspace))
            }
            Self::CancelReview { workspace, .. }
            | Self::ListReviews { workspace, .. }
            | Self::ShowReview { workspace, .. }
            | Self::AwaitReview { workspace, .. } => workspace.as_ref().map(Target::Space),
        }
    }
}
