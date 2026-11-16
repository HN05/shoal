//! Requests that launch agents.
use serde::{Deserialize, Serialize};

use super::Lifetime;
use crate::launches::Launch;

/// Requests that launch agents.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchMethod {
    /// Start an agent as `launch` asks, which a client made itself, so the
    /// journal records it as manual: the daemon decides its mode, the kind
    /// of space it starts in and the repository, creates or selects the
    /// space and queues its agent. Answers with [`super::Body::Launched`].
    Launch { launch: Box<Launch> },
}

impl LaunchMethod {
    pub fn lifetime(&self) -> Lifetime {
        match self {
            // A chat's start waits for requests of the chat start worker it
            // runs.
            Self::Launch { .. } => Lifetime::Open,
        }
    }
}
