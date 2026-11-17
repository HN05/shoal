//! Chats as the daemon starts them for a client.
use crate::agents::Delivery;
use crate::space_names::SpaceName;
use serde::{Deserialize, Serialize};
use shoal_ids::repository_ids::RepositoryId;

use shoal_ids::execution_ids::ExecutionId;

/// A chat started in a background place.
#[derive(Debug, Serialize, Deserialize)]
pub struct BackgroundChat {
    /// Its ID, once the place reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<shoal_ids::space_ids::SpaceId>,
    pub chat: SpaceName,
    pub agent: crate::agent::Agent,
    /// Its repository's ID; none for a chat without one.
    pub repository: Option<RepositoryId>,
    /// Where it runs and how to reach it, as the host that runs it says.
    pub place: String,
    /// What recording and linking it warned of, which its starter receives.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// A new chat recorded at once whose agent waits in the agent queue.
#[derive(Debug, Serialize, Deserialize)]
pub struct QueuedChat {
    pub chat: crate::model::Chat,
    pub entry: crate::model::QueueEntry,
    /// Entries waiting, this one included.
    pub waiting: usize,
    /// What recording and linking it warned of, which its starter receives.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// The chat whose agent a caller runs itself, recorded and linked, with
/// what that warned of, which its starter receives.
#[derive(Debug, Serialize, Deserialize)]
pub struct LaunchedChat {
    pub chat: crate::model::Chat,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// A new chat the daemon started for a client: in a background place, or
/// in an app that runs its agent detached, or recorded with its agent
/// queued.
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StartedChat {
    Queued(Box<QueuedChat>),
    Background(BackgroundChat),
    Detached(Box<crate::chats::DetachedChat>),
}

/// A chat started detached in an app.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetachedChat {
    pub chat: crate::model::Chat,
    pub execution_id: ExecutionId,
    pub pid: u32,
    pub log: std::path::PathBuf,
    #[serde(flatten)]
    pub delivery: Delivery,
    /// What recording and linking it warned of, which its starter receives.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}
