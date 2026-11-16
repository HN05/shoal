//! Where a space's agent is in its work: a status line in the agent's own
//! words, the steps of its plan, which its plan tool keeps through Shoal's
//! hooks or `shoal progress --steps` sets, and what blocks the space or its
//! steps, which `shoal progress --add-blocker` records.
use anyhow::Result;
use shoal_protocol::progress::{Progress, ProgressChange};

use crate::connection::{Daemon, request};
use shoal_protocol::{protocol::AgentMethod, space_names::SpaceSelector};

/// Apply `change` to the progress of `space`, answering with the progress
/// after it.
pub async fn set(
    daemon: &impl Daemon,
    space: SpaceSelector,
    change: ProgressChange,
) -> Result<Option<Progress>> {
    request(daemon, AgentMethod::SetProgress { space, change }).await
}

/// The progress of `space`.
pub async fn get(daemon: &impl Daemon, space: SpaceSelector) -> Result<Option<Progress>> {
    request(daemon, AgentMethod::Progress { space }).await
}
