use crate::space_names::SpaceSelector;
use serde::{Deserialize, Serialize};

/// Requests about the holds that keep spaces from cleanup.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(
    clippy::enum_variant_names,
    reason = "variants are named as their requests are spelled on the wire"
)]
pub enum HoldMethod {
    HoldAcquire {
        space: SpaceSelector,
        name: String,
        reason: Option<String>,
    },
    HoldRelease {
        space: SpaceSelector,
        name: String,
    },
    HoldList {
        space: SpaceSelector,
    },
}
