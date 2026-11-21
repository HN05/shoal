use super::Forwarding;
use crate::resources::ResourceRequest;
use crate::space_names::SpaceSelector;
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

impl ResourceMethod {
    /// A permit is a lease taken for the caller; what is leased and
    /// configured is state.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::ResourceOverview { .. } | Self::ResourceCatalog { .. } => Forwarding::Online,
            Self::ResourceAcquire { .. } | Self::ResourceRelease { .. } => Forwarding::Never,
        }
    }
}
