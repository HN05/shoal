//! What `shoal repo` prints.
use serde::{Deserialize, Serialize};

use std::path::PathBuf;

use crate::client::output::outputs;

#[derive(Serialize, Deserialize)]
pub struct LocalConfig {
    pub path: PathBuf,
    /// Unset while the file does not exist.
    pub toml: Option<String>,
}

outputs!(LocalConfig);
