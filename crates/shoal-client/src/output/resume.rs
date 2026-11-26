//! What `shoal resume` prints.
use serde::{Deserialize, Serialize};
use shoal_protocol::space_names::SpaceName;

use super::restore::Reopened;
use crate::output::{
    agents::{Agent, Skipped},
    batch::Failure,
    outputs,
};
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
    /// The other joined machine its space runs on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
}

/// What a resume did.
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
    /// The restores and launches it queued or started in the spaces of
    /// other joined machines, each in its machine's queue.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub elsewhere: Vec<QueuedElsewhere>,
    /// The spaces it left, as those of an offline machine.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<Skipped>,
    pub failed: Vec<Failure>,
}

/// The restores and launches a resume queued or started in a space of
/// another joined machine, which that machine's queue lists.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedElsewhere {
    pub space: SpaceName,
    pub machine: String,
    pub queued: Vec<QueueEntry>,
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
