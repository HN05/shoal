//! What chat commands print.
use crate::repository_ids::RepositoryId;
use crate::space_names::SpaceName;
use serde::{Deserialize, Serialize};

use crate::output::outputs;

/// A chat started in a background place.
#[derive(Debug, Serialize, Deserialize)]
pub struct BackgroundChat {
    pub chat: SpaceName,
    pub agent: crate::agent::Agent,
    /// Its repository's ID; none for a chat without one.
    pub repository: Option<RepositoryId>,
    /// Where it runs and how to reach it, as the host that runs it says.
    pub place: String,
}

/// A removed chat.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemovedChat {
    pub chat: crate::model::Chat,
    /// The spaces below it removed first, each after those below it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<SpaceName>,
}

/// A new chat recorded at once whose agent waits in the agent queue.
#[derive(Debug, Serialize, Deserialize)]
pub struct QueuedChat {
    pub chat: crate::model::Chat,
    pub entry: crate::model::QueueEntry,
    /// Entries waiting, this one included.
    pub waiting: usize,
}

/// A new chat the daemon started for a client: in a background place, or
/// in an app that runs its agent detached, or recorded with its agent
/// queued.
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StartedChat {
    Queued(Box<QueuedChat>),
    Background(BackgroundChat),
    Detached(Box<crate::output::agents::DetachedChat>),
}

outputs!(BackgroundChat, QueuedChat, RemovedChat, StartedChat);
