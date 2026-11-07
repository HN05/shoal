//! What `shoal config` prints.
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::output::outputs;

/// An edited config file and the backup of what it replaced.
#[derive(Debug, Serialize, Deserialize)]
pub struct Edited {
    pub config: PathBuf,
    pub backup: Option<PathBuf>,
    pub daemon_reloaded: bool,
}

/// The reference bundled with the release.
#[derive(Debug, Serialize, Deserialize)]
pub struct Reference {
    pub reference: String,
}

outputs!(Edited, Reference);
