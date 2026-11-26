use super::{Forwarding, Target};
use crate::model::PortRequest;
use crate::space_names::SpaceSelector;
use serde::{Deserialize, Serialize};

/// Requests about a workspace's TCP port reservations.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortMethod {
    PortAcquire {
        workspace: SpaceSelector,
        name: String,
        request: PortRequest,
    },
    PortRelease {
        workspace: SpaceSelector,
        name: String,
    },
    PortOverview {
        workspace: SpaceSelector,
    },
    PortCatalog {
        workspace: SpaceSelector,
    },
}

impl PortMethod {
    pub(super) fn reads_only(&self) -> bool {
        match self {
            Self::PortOverview { .. } | Self::PortCatalog { .. } => true,
            Self::PortAcquire { .. } | Self::PortRelease { .. } => false,
        }
    }
}

impl PortMethod {
    /// A port is a lease taken for the caller; what is reserved and
    /// configured is state.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::PortOverview { .. } | Self::PortCatalog { .. } => Forwarding::Online,
            Self::PortAcquire { .. } | Self::PortRelease { .. } => Forwarding::Never,
        }
    }

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::PortAcquire { workspace, .. }
            | Self::PortRelease { workspace, .. }
            | Self::PortOverview { workspace }
            | Self::PortCatalog { workspace } => Some(Target::Space(workspace)),
        }
    }
}
