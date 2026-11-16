//! A workspace's service containers, which one Compose project runs beside
//! it: how a repository and the machine configure them, and the project
//! Shoal records and reports.
use serde::{Deserialize, Serialize};
use shoal_states::states;

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
    /// with its setup, before it is ready.
    ContainerStart {
        #[default]
        Acquire => "acquire",
        Setup => "setup",
    }
);

/// `[machine.containers]`: the engine this machine runs projects with.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ContainerMachine {
    /// Unset takes the first of Docker and Podman on `PATH`.
    pub runtime: Option<ContainerRuntime>,
}
