//! Messages for the agents working in a workspace, from the user or the daemon.
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentMessage {
    pub id: i64,
    /// Unix seconds.
    pub created_at: i64,
    /// Empty when the message is only its attachments.
    pub message: String,
    /// Shoal's copies of the files attached to the message.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<PathBuf>,
}
