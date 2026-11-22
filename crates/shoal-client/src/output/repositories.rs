//! What `shoal repo` prints.
use serde::{Deserialize, Serialize};

use std::path::PathBuf;

use shoal_protocol::{hub::directory::RegisteredRepository, model::Repository};

use crate::output::outputs;

#[derive(Serialize, Deserialize)]
pub struct LocalConfig {
    pub path: PathBuf,
    /// Unset while the file does not exist.
    pub toml: Option<String>,
}

/// A repository `shoal repo ls` lists: this machine's, or another joined
/// machine's, which names its `machine`.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
pub enum ListedRepository {
    Here(Repository),
    Elsewhere(RepositoryElsewhere),
}

/// A repository another joined machine registers, as the hub lists it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepositoryElsewhere {
    /// The machine's name.
    pub machine: String,
    pub machine_id: String,
    #[serde(flatten)]
    pub registered: RegisteredRepository,
}

impl RepositoryElsewhere {
    pub fn name(&self) -> &str {
        &self.registered.repository.name
    }

    /// Where it is cloned from, or its name where it has no origin.
    pub fn source(&self) -> &str {
        let repository = &self.registered.repository;
        (repository.url.as_deref())
            .or(repository.origin.as_deref())
            .unwrap_or(&repository.name)
    }
}

outputs!(LocalConfig, ListedRepository, RepositoryElsewhere);
