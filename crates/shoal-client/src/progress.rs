//! Where a space's agent is in its work: a status line in the agent's own
//! words, the steps of its plan, which its plan tool keeps through Shoal's
//! hooks or `shoal progress --steps` sets, and what blocks the space or its
//! steps, which `shoal progress --add-blocker` records.
use shoal_protocol::progress::{Progress, ProgressChange};

use crate::connection::Prepared;
use shoal_protocol::{protocol::AgentMethod, space_names::SpaceSelector};

/// Apply `change` to the progress of `space`, answering with the progress
/// after it.
pub fn set(space: SpaceSelector, change: ProgressChange) -> Prepared<Option<Progress>> {
    Prepared::new(AgentMethod::SetProgress { space, change })
}

/// The progress of `space`.
pub fn get(space: SpaceSelector) -> Prepared<Option<Progress>> {
    Prepared::new(AgentMethod::Progress { space })
}
