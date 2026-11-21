//! What workspace commands print.
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::output::outputs;
use shoal_protocol::model::WorkspaceRef;

/// A workspace's directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Directory {
    pub path: PathBuf,
}

/// The workspace a workspace builds on, and those built on it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bases {
    pub base_workspace: Option<WorkspaceRef>,
    pub stacked_workspaces: Vec<WorkspaceRef>,
}

outputs!(Directory, Bases);
