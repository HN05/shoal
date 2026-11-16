use crate::space_names::SpaceSelector;
use serde::{Deserialize, Serialize};

/// Requests about a workspace's service containers.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerMethod {
    ContainerCatalog {
        workspace: SpaceSelector,
    },
    ContainerOverview {
        workspace: Option<SpaceSelector>,
    },
    AcquireContainers {
        workspace: SpaceSelector,
        reason: Option<String>,
    },
    ReleaseContainers {
        workspace: SpaceSelector,
    },
}
