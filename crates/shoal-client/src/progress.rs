//! Reading and changing a space's progress.
use shoal_protocol::progress::{Progress, ProgressChange};

use crate::connection::Prepared;
use shoal_protocol::{protocol::AgentMethod, space_names::SpaceSelector};

pub fn set(space: SpaceSelector, change: ProgressChange) -> Prepared<Option<Progress>> {
    Prepared::new(AgentMethod::SetProgress { space, change })
}

pub fn get(space: SpaceSelector) -> Prepared<Option<Progress>> {
    Prepared::new(AgentMethod::Progress { space })
}
