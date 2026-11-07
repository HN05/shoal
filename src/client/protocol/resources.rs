use crate::client::resources::ResourceRequest;
use crate::client::space_names::SpaceSelector;
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
        workspace: SpaceSelector,
        request: ResourceRequest,
    },
    ResourceRelease {
        workspace: SpaceSelector,
        pool: String,
        name: String,
    },
    ResourceOverview {
        workspace: SpaceSelector,
    },
    ResourceCatalog {
        workspace: SpaceSelector,
    },
}
