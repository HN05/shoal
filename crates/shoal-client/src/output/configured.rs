//! What `shoal run` lists of the configured commands.
use serde::{Deserialize, Serialize};

use crate::output::outputs;
use shoal_protocol::config::repo::ConfigLayer;

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandDefinition {
    pub name: String,
    pub argv: Vec<String>,
    pub layer: ConfigLayer,
}

outputs!(CommandDefinition);
