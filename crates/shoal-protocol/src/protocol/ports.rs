use super::Forwarding;
use crate::model::PortRequest;
use crate::space_names::SpaceSelector;
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
    /// A port is a lease taken for the caller; what is reserved and
    /// configured is state.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::PortOverview { .. } | Self::PortCatalog { .. } => Forwarding::Online,
            Self::PortAcquire { .. } | Self::PortRelease { .. } => Forwarding::Never,
        }
    }
}
