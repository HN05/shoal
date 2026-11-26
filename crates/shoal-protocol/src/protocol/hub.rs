//! Requests about the hub: those the hub answers itself, and that a joined
//! machine's daemon passes to it over its link.
use serde::{Deserialize, Serialize};

use super::{Forwarding, Lifetime, Target};
use crate::hub::pairing::PairingCode;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HubMethod {
    /// A new pairing code from the hub. Answers with
    /// [`super::Body::Pairing`].
    PairHub,
    /// Join the hub this machine reaches `via`, under `name`, or its host
    /// name. Answers with [`super::Body::Membership`].
    JoinHub {
        via: JoinVia,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// Leave the hub, revoking this machine's credential there. Answers
    /// with [`super::Body::LeftHub`].
    LeaveHub,
    /// The hub's listener and members. Answers with
    /// [`super::Body::HubOverview`].
    HubMembers,
    /// Install the latest release on the hub, where it was downloaded.
    /// Answers with [`super::Body::HubUpdate`].
    UpdateHub,
    /// Revoke the credential of the member `member` names by ID or name.
    /// Answers with [`super::Body::HubRevoked`].
    RevokeHubMember { member: String },
    /// Long-lived, on the hub's machine: register the caller as the placer
    /// an `external` placement policy asks, in place of the one before,
    /// until it hangs up. Answers with [`super::Body::Ok`], then each
    /// start to place as [`super::Body::PlacementAsked`].
    ServePlacement,
    /// The placer's placement of the snapshot `number`, which the hub
    /// waits for briefly. Answers with [`super::Body::Ok`].
    AnswerPlacement {
        number: u64,
        placement: shoal_schedule::placement::Placement,
    },
}

/// How a joining machine reaches the hub.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JoinVia {
    /// The hub's URL, with the code that admits it and the hub's key.
    Code(PairingCode),
    /// A command whose standard input and output relay the hub daemon's
    /// socket.
    Command(Vec<String>),
}

impl HubMethod {
    pub(super) fn reads_only(&self) -> bool {
        match self {
            Self::HubMembers => true,
            Self::PairHub
            | Self::JoinHub { .. }
            | Self::LeaveHub
            | Self::UpdateHub
            | Self::RevokeHubMember { .. }
            | Self::ServePlacement
            | Self::AnswerPlacement { .. } => false,
        }
    }
}

impl HubMethod {
    pub fn lifetime(&self) -> Lifetime {
        match self {
            Self::ServePlacement => Lifetime::Open,
            _ => Lifetime::Reply,
        }
    }

    /// Each is about the caller's machine's membership, or its hub role.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::PairHub
            | Self::JoinHub { .. }
            | Self::LeaveHub
            | Self::HubMembers
            | Self::UpdateHub
            | Self::RevokeHubMember { .. }
            | Self::ServePlacement
            | Self::AnswerPlacement { .. } => Forwarding::Never,
        }
    }

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::PairHub
            | Self::JoinHub { .. }
            | Self::LeaveHub
            | Self::HubMembers
            | Self::UpdateHub
            | Self::RevokeHubMember { .. }
            | Self::ServePlacement
            | Self::AnswerPlacement { .. } => None,
        }
    }
}
