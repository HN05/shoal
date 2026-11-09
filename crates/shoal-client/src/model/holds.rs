//! Holds that keep a workspace from cleanup.
use crate::space_ids::SpaceId;
use serde::{Deserialize, Serialize};

/// A caller-named claim that a workspace is still in use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceHold {
    pub workspace_id: SpaceId,
    pub name: String,
    pub reason: Option<String>,
    pub created_at: crate::time::UnixTime,
    #[serde(default)]
    pub placed_by: HoldPlacer,
}

crate::state::states!(
    /// Who placed a hold, which decides who may release it.
    #[derive(Default)]
    HoldPlacer {
        /// The user or a host app; only they release it.
        #[default]
        User => "user",
        /// An agent, whose workspace or one above it may release it.
        Agent => "agent",
    }
);
