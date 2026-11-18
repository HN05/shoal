//! Requests about the hub: those the hub answers itself, and that a joined
//! machine's daemon passes to it over its link.
use serde::{Deserialize, Serialize};

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
    /// Revoke the credential of the member `member` names by ID or name.
    /// Answers with [`super::Body::HubMember`].
    RevokeHubMember { member: String },
    /// The other joined machines' spaces the caller sees, from the hub's
    /// directory. Answers with the pages of [`super::Body::HubView`], none
    /// on a machine that is not the hub and joined none.
    HubDirectory,
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
