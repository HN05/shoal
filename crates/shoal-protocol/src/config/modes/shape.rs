//! The space each mode starts, which a start on another machine reads of
//! that machine's modes.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::model::SpaceKind;

/// Each mode by name, with the space it starts.
pub type ModeShapes = BTreeMap<String, ModeShape>;

/// The space a mode starts: of `kind`, and in a repository or without one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModeShape {
    pub kind: SpaceKind,
    pub repository: bool,
}
