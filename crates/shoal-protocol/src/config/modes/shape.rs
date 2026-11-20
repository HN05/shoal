//! The space each mode starts and the label that calls for it, which a
//! start on another machine reads of that machine's modes.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::model::SpaceKind;

/// Each mode by name, with the space it starts and its label.
pub type ModeShapes = BTreeMap<String, ModeShape>;

/// The space a mode starts: of `kind`, and in a repository or without one;
/// with the forge label of the issues and PRs a launch without a mode
/// starts in it, if it has one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModeShape {
    pub kind: SpaceKind,
    pub repository: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}
