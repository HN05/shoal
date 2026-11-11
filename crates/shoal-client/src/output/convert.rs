//! What `shoal convert` prints.
use serde::{Deserialize, Serialize};

use crate::{model::Workspace, output::outputs};

/// A converted space: the workspace a chat became, set up, or the
/// workspace that becomes a chat once its agent's turn ends.
#[derive(Serialize, Deserialize)]
pub struct Converted {
    /// What the space is, or becomes: a workspace or a chat.
    pub kind: crate::model::SpaceKind,
    #[serde(flatten)]
    pub workspace: Workspace,
}

outputs!(Converted);
