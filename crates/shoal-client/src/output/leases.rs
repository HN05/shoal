//! What `shoal leases` prints.
use serde::{Deserialize, Serialize};

use crate::{
    model::{PortOverview, PortSuggestion, Workspace},
    output::outputs,
    resources::Overview,
    sim::SimulatorOverview,
};

/// Every kind's configuration and leases for one workspace.
#[derive(Debug, Serialize, Deserialize)]
pub struct Leases {
    pub workspace: Workspace,
    pub ports: PortOverview,
    pub simulators: SimulatorOverview,
    pub resources: Overview,
}

/// One lease or active approval request a workspace holds, named as its
/// release request needs it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Held {
    Port { name: String },
    Sim { name: String },
    Resource { pool: String, name: String },
    Repo { pool: String, name: String },
    Secret { name: String },
}

impl std::fmt::Display for Held {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Port { name } => write!(f, "port {name}"),
            Self::Sim { name } => write!(f, "simulator {name}"),
            Self::Resource { pool, name } => write!(f, "resource {pool}/{name}"),
            Self::Repo { pool, name } => write!(f, "repo {pool}/{name}"),
            Self::Secret { name } => write!(f, "secret {name}"),
        }
    }
}

/// The leases and active approval requests a release released.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Releases {
    pub released: Vec<Held>,
}

/// A port reservation refused for a conflict, with the port suggested
/// instead.
#[derive(Debug, Serialize, Deserialize)]
pub struct Suggested {
    #[serde(flatten)]
    pub suggestion: PortSuggestion,
    /// Always false.
    pub reserved: bool,
}

outputs!(Leases, Releases, Suggested);
