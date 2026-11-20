//! Agents' changes: the hunks Shoal records as the agents it runs change
//! the files of their worktrees.
use anyhow::Result;
use shoal_protocol::{
    changes::{Change, ToolChange},
    protocol::ChangeMethod,
};

use crate::connection::{Daemon, request};

/// Record what a tool call of the caller's agent changed, as its hook
/// reports it; the changes Shoal could not tell were the agent's.
pub async fn record(daemon: &impl Daemon, change: ToolChange) -> Result<Vec<Change>> {
    request(daemon, ChangeMethod::RecordChange { change }).await
}
