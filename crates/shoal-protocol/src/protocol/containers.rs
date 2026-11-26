use super::{Forwarding, Target};
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
    ContainerAcquire {
        workspace: SpaceSelector,
        reason: Option<String>,
    },
    ContainerRelease {
        workspace: SpaceSelector,
    },
    /// Start a workspace's containers after its setup command, where its
    /// repository starts them with setup: for the agent that starts there
    /// next when `for_agent`, else for the caller.
    ContainerSetUp {
        workspace: SpaceSelector,
        for_agent: bool,
    },
    /// The caller's agent starts in the workspace: where its repository
    /// starts its containers with setup, they become the agent's, starting
    /// them if they do not run.
    ContainerAgentStart {
        workspace: SpaceSelector,
    },
}

impl ContainerMethod {
    pub(super) fn reads_only(&self) -> bool {
        match self {
            Self::ContainerCatalog { .. } | Self::ContainerOverview { .. } => true,
            Self::ContainerAcquire { .. }
            | Self::ContainerRelease { .. }
            | Self::ContainerSetUp { .. }
            | Self::ContainerAgentStart { .. } => false,
        }
    }
}

impl ContainerMethod {
    /// Containers are a lease taken for the caller, and their setup the
    /// workspace's own; what the repository configures and what runs are
    /// state.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::ContainerCatalog { .. } | Self::ContainerOverview { .. } => Forwarding::Online,
            Self::ContainerAcquire { .. }
            | Self::ContainerRelease { .. }
            | Self::ContainerSetUp { .. }
            | Self::ContainerAgentStart { .. } => Forwarding::Never,
        }
    }

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::ContainerCatalog { workspace }
            | Self::ContainerOverview {
                workspace: Some(workspace),
            }
            | Self::ContainerAcquire { workspace, .. }
            | Self::ContainerRelease { workspace }
            | Self::ContainerSetUp { workspace, .. }
            | Self::ContainerAgentStart { workspace } => Some(Target::Space(workspace)),
            Self::ContainerOverview { workspace: None } => None,
        }
    }
}
