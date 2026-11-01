//! What issue and PR commands print.
use serde::{Deserialize, Serialize};

use crate::{client::output::outputs, forge::items::item::Opened, model::Workspace};

/// A revert and, when Git made it, the workspace its branch is in.
#[derive(Serialize, Deserialize)]
pub struct Revert {
    #[serde(flatten)]
    pub opened: Opened,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<Workspace>,
}

outputs!(Revert);
