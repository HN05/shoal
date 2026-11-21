//! What issue and PR commands print, and what linking one does.
use serde::{Deserialize, Serialize};

use crate::{forge::items::item::Opened, output::outputs};
use shoal_protocol::model::Workspace;

/// A revert and, when Git made it, the workspace its branch is in.
#[derive(Serialize, Deserialize)]
pub struct Revert {
    #[serde(flatten)]
    pub opened: Opened,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<Workspace>,
}

/// Whether an item is linked, and how, with what assigning or unassigning
/// an issue on the forge warned of.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Registered {
    pub registered: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relation: Option<shoal_protocol::model::Relation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

outputs!(Revert, Registered);
