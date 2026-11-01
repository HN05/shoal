//! What `shoal catalog` prints.
use serde::{Deserialize, Serialize};

use crate::{
    client::{
        output::outputs, resources::Catalog as ResourceCatalog, secrets::SecretCatalog,
        sim::SimulatorCatalog,
    },
    model::PortCatalog,
};

/// Every kind's catalog for one workspace.
#[derive(Debug, Serialize, Deserialize)]
pub struct Catalog {
    pub ports: PortCatalog,
    pub simulators: SimulatorCatalog,
    pub resources: ResourceCatalog,
    pub secrets: SecretCatalog,
}

outputs!(Catalog);

outputs!(
    PortCatalog,
    ResourceCatalog,
    SecretCatalog,
    SimulatorCatalog
);
