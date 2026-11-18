//! Chats as the daemon records them for a client.
use serde::{Deserialize, Serialize};

/// The chat whose agent a caller runs itself, recorded and linked, with
/// what that warned of, which its starter receives.
#[derive(Debug, Serialize, Deserialize)]
pub struct LaunchedChat {
    pub chat: crate::model::Chat,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}
