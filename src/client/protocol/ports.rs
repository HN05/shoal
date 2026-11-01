use crate::daemon::ports::PortRequest;
use serde::{Deserialize, Serialize};

/// Requests about a workspace's TCP port reservations.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(
    clippy::enum_variant_names,
    reason = "variants are named as their requests are spelled on the wire"
)]
pub enum PortMethod {
    PortAcquire {
        workspace: String,
        name: String,
        request: PortRequest,
    },
    PortRelease {
        workspace: String,
        name: String,
    },
    PortOverview {
        workspace: String,
    },
    PortCatalog {
        workspace: String,
    },
}
