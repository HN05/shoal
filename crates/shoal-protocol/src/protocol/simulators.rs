use crate::sim::SimRequest;
use crate::space_names::SpaceSelector;
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
        workspace: SpaceSelector,
    },
    SimOverview {
        workspace: Option<SpaceSelector>,
    },
    /// Lease records for completion, without loading repository configuration.
    SimList {
        workspace: Option<SpaceSelector>,
    },
    SimAcquire {
        workspace: SpaceSelector,
        request: SimRequest,
    },
    SimRelease {
        workspace: SpaceSelector,
        name: String,
    },
    SimHistory {
        workspace: Option<SpaceSelector>,
        limit: u32,
        before: Option<i64>,
    },
}
