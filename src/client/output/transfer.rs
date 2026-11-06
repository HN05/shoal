//! What `shoal move` prints.
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::client::{
    output::{batch::Failure, chat::BackgroundChat, outputs, resume::Reopened},
    removal::RemovalResult,
    transfer::Manifest,
};

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

/// A space an import continued: a workspace opened again, or a chat started
/// in the background.
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Imported {
    Workspace(Box<Reopened>),
    Chat(BackgroundChat),
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
    pub imported: Vec<Imported>,
    pub failed: Vec<ImportFailure>,
}

/// The spaces a move to another machine continued there.
#[derive(Debug, Serialize, Deserialize)]
pub struct Moves {
    pub moved: Vec<Imported>,
    pub failed: Vec<Failure>,
}

/// The copies a copy to another machine opened there.
#[derive(Debug, Serialize, Deserialize)]
pub struct Copies {
    pub copied: Vec<Imported>,
    pub failed: Vec<Failure>,
}

outputs!(Exported, Exports, Imported, Imports, Moves, Copies);
