//! What chat commands print.
use crate::repository_ids::RepositoryId;
use serde::{Deserialize, Serialize};

use crate::output::outputs;

/// A chat started in a background place.
#[derive(Debug, Serialize, Deserialize)]
pub struct BackgroundChat {
    pub chat: String,
    pub agent: String,
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
    pub children: Vec<String>,
}

outputs!(BackgroundChat, RemovedChat);
