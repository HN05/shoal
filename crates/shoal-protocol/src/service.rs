//! The daemon's service definition, which a service manager starts it by.
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A service definition, as `shoal daemon install --dry-run` prints it.
#[derive(Serialize, Deserialize)]
pub struct Definition {
    pub path: PathBuf,
    pub content: String,
}
