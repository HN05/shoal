use super::{Forwarding, Target};
use crate::sim::SimRequest;
use crate::space_names::SpaceSelector;
use serde::{Deserialize, Serialize};

/// Requests about simulator leases.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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

impl SimulatorMethod {
    /// A simulator is a lease taken for the caller; what is leased,
    /// configured and recorded is state.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::SimCatalog { .. }
            | Self::SimOverview { .. }
            | Self::SimList { .. }
            | Self::SimHistory { .. } => Forwarding::Online,
            Self::SimAcquire { .. } | Self::SimRelease { .. } => Forwarding::Never,
        }
    }

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::SimCatalog { workspace }
            | Self::SimAcquire { workspace, .. }
            | Self::SimRelease { workspace, .. } => Some(Target::Space(workspace)),
            Self::SimOverview { workspace }
            | Self::SimList { workspace }
            | Self::SimHistory { workspace, .. } => workspace.as_ref().map(Target::Space),
        }
    }
}
