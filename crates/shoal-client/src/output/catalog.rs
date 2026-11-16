//! What `shoal catalog` prints.
use serde::{Deserialize, Serialize};

use crate::output::outputs;
use shoal_protocol::{
    containers::ContainerCatalog, model::PortCatalog, resources::Catalog as ResourceCatalog,
    secrets::SecretCatalog, sim::SimulatorCatalog,
};

/// Every kind's catalog for one workspace.
#[derive(Debug, Serialize, Deserialize)]
pub struct Catalog {
    pub ports: PortCatalog,
    pub simulators: SimulatorCatalog,
    pub containers: ContainerCatalog,
    pub resources: ResourceCatalog,
    pub secrets: SecretCatalog,
}

outputs!(Catalog);

outputs!(
    ContainerCatalog,
    PortCatalog,
    ResourceCatalog,
    SecretCatalog,
    SimulatorCatalog
);
