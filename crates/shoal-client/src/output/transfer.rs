//! What `shoal move` prints.
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::output::{batch::Failure, outputs, restore::Reopened};
use shoal_protocol::{removal::RemovalResult, transfer::Manifest};

/// A space written to a package, and its removal here.
#[derive(Debug, Serialize, Deserialize)]
pub struct Exported {
    pub file: PathBuf,
    pub manifest: Manifest,
    #[serde(default)]
    pub removal: Option<RemovalResult>,
}

/// The spaces an export of several wrote into `directory`.
#[derive(Debug, Serialize, Deserialize)]
pub struct Exports {
    pub exported: Vec<Exported>,
    pub failed: Vec<Failure>,
    pub directory: PathBuf,
}

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

/// The spaces a move to another machine continued there.
#[derive(Debug, Serialize, Deserialize)]
pub struct Moves {
    pub moved: Vec<Reopened>,
    pub failed: Vec<Failure>,
}

/// The copies a copy to another machine opened there.
#[derive(Debug, Serialize, Deserialize)]
pub struct Copies {
    pub copied: Vec<Reopened>,
    pub failed: Vec<Failure>,
}

outputs!(Exported, Exports, Imports, Moves, Copies);
