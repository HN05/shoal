//! What `shoal forge download` prints.
use serde::{Deserialize, Serialize};

use crate::output::outputs;

/// Where a download was written, for `--json`.
#[derive(Serialize, Deserialize)]
pub struct Written {
    pub path: std::path::PathBuf,
    pub bytes: usize,
    pub content_type: Option<String>,
}

outputs!(Written);
