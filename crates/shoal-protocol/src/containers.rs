//! A workspace's service containers: their config, and the Compose project
//! Shoal records.
use serde::{Deserialize, Serialize};
use shoal_ids::space_ids::SpaceId;
use shoal_states::states;

use crate::acquirers::Acquirer;

states!(
    /// The container engine whose `compose` runs a workspace's project.
    ContainerRuntime {
        Docker => "docker",
        Podman => "podman",
    }
);

impl ContainerRuntime {
    /// The engines in the order Shoal looks for one on `PATH`.
    pub const ALL: [Self; 2] = [Self::Docker, Self::Podman];
}

states!(
    #[derive(Default)]
    /// When a workspace's project starts: on `shoal acquire containers`, or
    /// once setup readies the workspace, before its post-setup hook and
    /// agent.
    ContainerStart {
        #[default]
        Acquire => "acquire",
        Setup => "setup",
    }
);

states!(
    /// Where a recorded project is, as Shoal last left it.
    ContainerState {
        /// Recorded before `compose up`, which has not finished.
        Starting => "starting",
        Running => "running",
        /// Stopped by a release, its volumes kept.
        Stopped => "stopped",
        /// `compose up` failed, perhaps after some services started; the
        /// record names why.
        Failed => "failed",
    }
);

impl ContainerState {
    /// Whether the project may hold containers that run, as a failed `up`
    /// may leave some: a lease that keeps the workspace and ends with its
    /// acquirer.
    pub fn active(self) -> bool {
        matches!(self, Self::Starting | Self::Running | Self::Failed)
    }
}

/// A workspace's Compose project, as Shoal records it.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct ContainerProject {
    pub workspace_id: SpaceId,
    /// The Compose project name, which only this workspace's project has.
    pub project: String,
    pub runtime: ContainerRuntime,
    /// The Compose file it started from, relative to the worktree.
    pub compose: String,
    pub state: ContainerState,
    pub acquirer: Acquirer,
    pub reason: Option<String>,
    /// Why the last `compose up` failed.
    pub error: Option<String>,
}

/// One service of a running project, as the runtime lists it.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct ContainerService {
    pub service: String,
    pub state: String,
    pub ports: Vec<PublishedPort>,
}

/// A container port the runtime published on this machine.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct PublishedPort {
    pub target: u16,
    pub published: u16,
    pub protocol: String,
}

/// A workspace's project with its services as the runtime lists them, or
/// why it could not.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Containers {
    #[serde(flatten)]
    pub project: ContainerProject,
    pub services: Vec<ContainerService>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub services_error: Option<String>,
}

/// The projects of one workspace or of every one.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct ContainerOverview {
    pub projects: Vec<Containers>,
}

/// What a workspace's repository and this machine configure for its
/// containers, and whether the runtime can run them.
#[derive(Debug, Deserialize, Serialize)]
pub struct ContainerCatalog {
    /// The Compose file; none gives the workspace no containers.
    pub compose: Option<String>,
    pub ports: Vec<String>,
    pub start: ContainerStart,
    /// The runtime that runs the project, which the machine names or Shoal
    /// found on `PATH`.
    pub runtime: Option<ContainerRuntime>,
    /// Why no runtime can run the project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
}
