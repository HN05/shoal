//! What `shoal hub` prints, and what a command for an offline machine's
//! space prints while the hub holds its request.
use serde::{Deserialize, Serialize};
use shoal_protocol::hub::pairing::Pairing;

use crate::output::outputs;

/// A pairing code `shoal hub pair` printed, with what the user should know
/// before using it, such as that it would cross the network unencrypted.
#[derive(Debug, Serialize, Deserialize)]
pub struct Paired {
    #[serde(flatten)]
    pub pairing: Pairing,
    /// The command that installs the hub's version on a computer without
    /// Shoal and joins it with the code.
    pub installer: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

outputs!(
    Paired,
    shoal_protocol::hub::HubUpdate,
    shoal_protocol::hub::Left,
    shoal_protocol::hub::Member,
    shoal_protocol::hub::Membership,
    shoal_protocol::hub::Overview,
    shoal_protocol::hub::placement::Decision,
    shoal_protocol::hub::routing::Queued,
);
