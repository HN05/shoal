use crate::daemon::resources::ResourceRequest;
use serde::{Deserialize, Serialize};

/// Requests about leases of cooperative resources.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(
    clippy::enum_variant_names,
    reason = "variants are named as their requests are spelled on the wire"
)]
pub enum ResourceMethod {
    ResourceAcquire {
        workspace: String,
        request: ResourceRequest,
    },
    ResourceRelease {
        workspace: String,
        pool: String,
        name: String,
    },
    ResourceOverview {
        workspace: String,
    },
    ResourceCatalog {
        workspace: String,
    },
}
