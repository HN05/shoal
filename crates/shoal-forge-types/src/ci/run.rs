//! A CI workflow run Shoal started.

use serde::{Deserialize, Serialize};

/// A started run, with its web URL where the forge reports one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartedRun {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    pub branch: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The workspace linked the run, so watches follow it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub linked: bool,
    /// What went wrong after the run started, such as linking it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}
