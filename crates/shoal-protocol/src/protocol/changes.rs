use serde::{Deserialize, Serialize};

use super::{Forwarding, Target};
use crate::changes::ToolChange;

/// Requests about agents' changes: recording them from an agent's hooks.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeMethod {
    /// What a tool call of the caller's agent changed in its space's
    /// worktree, from the agent's hook; the mixed changes it recorded.
    RecordChange { change: ToolChange },
}

impl ChangeMethod {
    /// A hook records its own agent's changes where it runs.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::RecordChange { .. } => Forwarding::Never,
        }
    }

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::RecordChange { .. } => None,
        }
    }
}
