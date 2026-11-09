//! Completion signals and the files they hand back.
use serde::{Deserialize, Serialize};

/// A file a completion handed back: the record of Shoal's copy, which tells a
/// changed file from the one handed back before, and its name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandedBack {
    pub id: i64,
    pub name: String,
}

/// An assignment completion signal, bound to the workspace's HEAD.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Completion {
    pub head: String,
    pub cleanup: bool,
    /// Another branch has HEAD's work under other commits, so cleanup need
    /// not wait for them to be pushed.
    #[serde(default)]
    pub integrated: bool,
    /// The files the assignment handed back, which `shoal files` lists with
    /// Shoal's copies.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub results: Vec<HandedBack>,
    /// One line saying what the work found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub error: Option<String>,
}
