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
    /// Start a workspace's containers after its setup command, where its
    /// repository starts them with setup: for the agent that starts there
    /// next when `for_agent`, else for the caller.
    SetUpContainers {
        workspace: SpaceSelector,
        for_agent: bool,
    },
    /// The caller's agent starts in the workspace: where its repository
    /// starts its containers with setup, they become the agent's, starting
    /// them if they do not run.
    StartAgentContainers {
        workspace: SpaceSelector,
    },
}
