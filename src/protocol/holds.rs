use serde::{Deserialize, Serialize};

/// Requests about the holds that keep workspaces from cleanup.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(
    clippy::enum_variant_names,
    reason = "variants are named as their requests are spelled on the wire"
)]
pub enum HoldMethod {
    HoldAcquire {
        workspace: String,
        name: String,
        reason: Option<String>,
    },
    HoldRelease {
        workspace: String,
        name: String,
    },
    HoldList {
        workspace: String,
    },
}
