//! Holds that keep a space from cleanup.
use serde::{Deserialize, Serialize};
use shoal_ids::space_ids::SpaceId;

/// A caller-named claim that a space is still in use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceHold {
    pub space_id: SpaceId,
    pub name: String,
    pub reason: Option<String>,
    pub created_at: crate::time::UnixTime,
    #[serde(default)]
    pub placed_by: HoldPlacer,
}

shoal_states::states!(
    /// Who placed a hold, which decides who may release it.
    #[derive(Default)]
    HoldPlacer {
        /// The user or a host app; only they release it.
        #[default]
        User => "user",
        /// An agent, whose space or one above it may release it.
        Agent => "agent",
    }
);
