//! What `shoal update` prints.
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::client::output::outputs;

/// The executable `shoal update` replaced, or found current.
#[derive(Debug, Serialize, Deserialize)]
pub struct Update {
    pub updated: bool,
    /// The version it replaced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<String>,
    pub version: String,
    /// The latest release, when it was not newer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest: Option<String>,
    pub path: PathBuf,
}

outputs!(Update);
