//! What `shoal hub` prints.
use serde::{Deserialize, Serialize};
use shoal_protocol::hub::pairing::Pairing;

use crate::output::outputs;

/// A pairing code `shoal hub pair` printed, with what the user should know
/// before using it, such as that it would cross the network unencrypted.
#[derive(Debug, Serialize, Deserialize)]
pub struct Paired {
    #[serde(flatten)]
    pub pairing: Pairing,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

outputs!(
    Paired,
    shoal_protocol::hub::Left,
    shoal_protocol::hub::Member,
    shoal_protocol::hub::Membership,
    shoal_protocol::hub::Overview,
);
