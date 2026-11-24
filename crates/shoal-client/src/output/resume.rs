//! What `shoal resume` prints.
use serde::{Deserialize, Serialize};
use shoal_protocol::space_names::SpaceName;

use super::restore::Reopened;
use crate::output::{agents::Agent, batch::Failure, outputs};
use shoal_protocol::{
    model::{QueueEntry, Space},
    saved::SavedSummary,
};

/// A workspace's stopped commands, which Shoal reports rather than reruns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoppedCommands {
    pub workspace: SpaceName,
    /// Each command's arguments.
    pub commands: Vec<Vec<String>>,
}

/// The restores a resume queued, the commands it reported, the spaces
/// reopened for it, the interrupted turns it continued and the spaces it
/// failed for.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Queued {
    pub queued: Vec<QueueEntry>,
    pub commands: Vec<StoppedCommands>,
    /// The removed spaces it opened again.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reopened: Vec<Reopened>,
    /// The agents whose interrupted turns it continued.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub continued: Vec<Agent>,
    pub failed: Vec<Failure>,
}

/// A space whose agent can resume, and what of it resumes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumableSpace {
    pub space: Space,
    /// What it saved: stopped agents, last sessions and stopped commands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved: Option<SavedSummary>,
    /// Its launch that failed to start, which resumes by queuing again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_launch: Option<QueueEntry>,
}

impl ResumableSpace {
    /// Whether it holds unfinished work: a stopped agent or command, or a
    /// failed launch, rather than only last sessions.
    pub fn unfinished(&self) -> bool {
        self.failed_launch.is_some() || self.saved.as_ref().is_some_and(|saved| saved.unfinished)
    }
}

/// What `shoal resume` without a space picks among: the spaces whose agent
/// can resume, unfinished work first.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resumable {
    pub spaces: Vec<ResumableSpace>,
}

outputs!(StoppedCommands, Queued, Resumable);
