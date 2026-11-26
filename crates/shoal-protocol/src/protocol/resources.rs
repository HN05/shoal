use super::{Forwarding, Target};
use crate::resources::ResourceRequest;
use crate::space_names::SpaceSelector;
use serde::{Deserialize, Serialize};

/// Requests about leases of cooperative resources.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
    pub(super) fn reads_only(&self) -> bool {
        match self {
            Self::ResourceOverview { .. } | Self::ResourceCatalog { .. } => true,
            Self::ResourceAcquire { .. } | Self::ResourceRelease { .. } => false,
        }
    }
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

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::ResourceAcquire { workspace, .. }
            | Self::ResourceRelease { workspace, .. }
            | Self::ResourceOverview { workspace }
            | Self::ResourceCatalog { workspace } => Some(Target::Space(workspace)),
        }
    }
}
