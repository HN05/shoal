//! What `shoal shell` prints.
use serde::{Deserialize, Serialize};

use crate::client::output::outputs;

/// A script for the shell to evaluate.
#[derive(Debug, Serialize, Deserialize)]
pub struct Script {
    pub script: String,
}

outputs!(Script);
