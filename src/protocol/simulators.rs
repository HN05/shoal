use crate::sim::SimRequest;
use serde::{Deserialize, Serialize};

/// Requests about simulator leases.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(
    clippy::enum_variant_names,
    reason = "variants are named as their requests are spelled on the wire"
)]
pub enum SimulatorMethod {
    SimCatalog {
        workspace: String,
    },
    SimOverview {
        workspace: Option<String>,
    },
    /// Lease records for completion, without loading repository configuration.
    SimList {
        workspace: Option<String>,
    },
    SimAcquire {
        workspace: String,
        request: SimRequest,
    },
    SimRelease {
        workspace: String,
        name: String,
    },
    SimHistory {
        workspace: Option<String>,
        limit: u32,
        before: Option<i64>,
    },
}
