//! What `shoal move` and the imports of its packages print.
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::output::{outputs, restore::Reopened};
use shoal_protocol::transfer::{DeferredMove, ImportedMove, MoveReport};

/// A package an import of several could not import.
#[derive(Debug, Serialize, Deserialize)]
pub struct ImportFailure {
    pub file: PathBuf,
    pub error: String,
}

/// The spaces an import of several continued.
#[derive(Debug, Serialize, Deserialize)]
pub struct Imports {
    pub imported: Vec<Reopened>,
    pub failed: Vec<ImportFailure>,
}

outputs!(DeferredMove, Imports, ImportedMove, MoveReport);
