//! Requests about the hub: those the hub answers itself, and that a joined
//! machine's daemon passes to it over its link.
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HubMethod {
    /// A new pairing code from the hub. Answers with
    /// [`super::Body::Pairing`].
    PairHub,
    /// The hub's listener and members. Answers with
    /// [`super::Body::HubOverview`].
    HubMembers,
    /// Revoke the credential of the member `member` names by ID or name.
    /// Answers with [`super::Body::HubMember`].
    RevokeHubMember { member: String },
}
