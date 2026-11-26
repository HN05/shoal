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
    /// Start the caller's pending review of the workspace, or return the one
    /// it has. The threads it opens towards that workspace join it.
    StartReview { workspace: SpaceSelector },
    /// The caller's pending reviews, oldest first, of every machine that
    /// runs a space it sees; forwarded from another machine, those this
    /// machine keeps.
    PendingReviews,
    /// Submit the caller's pending review of the workspace, else its only
    /// one on any machine, at the workspace's HEAD.
    SubmitReview {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<SpaceSelector>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        approve: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },
    /// Cancel review `id`, else the caller's pending review of the
    /// workspace or its queued or running one; a pending review goes with
    /// its threads.
    CancelReview {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<SpaceSelector>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<ReviewId>,
        /// Whether the workspace is the one the command runs in, rather than
        /// one it names: with nothing to cancel there, the caller's only
        /// pending review, on any machine, goes.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        here: bool,
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
            Self::RunReview { .. }
            | Self::StartReview { .. }
            | Self::PendingReviews
            | Self::SubmitReview { .. }
            | Self::CancelReview { .. }
            | Self::ListReviews { .. }
            | Self::ShowReview { .. }
            | Self::CheckPrGate { .. } => Lifetime::Reply,
        }
    }
}

/// A piece of the JSON of a review response, which the client joins.
#[derive(Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReviewPiece(pub String);

impl ReviewMethod {
    pub(super) fn reads_only(&self) -> bool {
        match self {
            Self::PendingReviews => true,
            Self::RunReview { .. }
            | Self::StartReview { .. }
            | Self::SubmitReview { .. }
            | Self::CancelReview { .. }
            | Self::ListReviews { .. }
            | Self::ShowReview { .. }
            | Self::AwaitReview { .. }
            | Self::CheckPrGate { .. } => false,
        }
    }
}

impl ReviewMethod {
    /// A review runs on its workspace's machine, which keeps its threads,
    /// and a wait's answer comes as that machine writes it.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::RunReview { .. }
            | Self::StartReview { .. }
            | Self::SubmitReview { .. }
            | Self::CancelReview { .. }
            | Self::ListReviews { .. }
            | Self::ShowReview { .. }
            | Self::CheckPrGate { .. } => Forwarding::Online,
            Self::AwaitReview { .. } => Forwarding::Streamed,
            // Without a target, it goes only where the caller's machine
            // asks each machine for those it keeps.
            Self::PendingReviews => Forwarding::Online,
        }
    }

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::RunReview { workspace, .. }
            | Self::StartReview { workspace }
            | Self::CheckPrGate { workspace } => Some(Target::Space(workspace)),
            Self::PendingReviews => None,
            Self::CancelReview { workspace, .. }
            | Self::SubmitReview { workspace, .. }
            | Self::ListReviews { workspace, .. }
            | Self::ShowReview { workspace, .. }
            | Self::AwaitReview { workspace, .. } => workspace.as_ref().map(Target::Space),
        }
    }
}
