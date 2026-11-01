//! What `shoal run` lists of the configured commands.
use serde::{Deserialize, Serialize};

use crate::{client::output::outputs, config::repo::ConfigLayer};

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandDefinition {
    pub name: String,
    pub argv: Vec<String>,
    pub layer: ConfigLayer,
    pub bare_name: BareName,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BareName {
    Shorthand,
    BuiltIn,
}

outputs!(CommandDefinition, BareName);
