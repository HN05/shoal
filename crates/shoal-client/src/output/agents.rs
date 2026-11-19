//! What commands that act on running agents print: `shoal stop`, `shoal
//! restart` and the turn commands.
use serde::{Deserialize, Serialize};
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::agents::Delivery;
use shoal_protocol::space_names::SpaceName;

use crate::output::{batch::Failure, outputs};
use shoal_protocol::{model::QueueEntry, processes::Stopped};

/// An agent, by the name of its space, a workspace or a chat, and its own,
/// which is unknown for an execution whose wrapper is gone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    pub space: SpaceName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<shoal_protocol::agent::Agent>,
}

/// A stopped workspace's processes, or a stopped chat.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaceStopped {
    /// Whether its processes stopped; false for a CI job's workspace, whose
    /// run the forge was asked to cancel instead.
    pub stopped: bool,
    /// Whether agents, commands or the chat wait for `shoal restore`.
    pub saved: bool,
    /// How many processes Shoal does not track it stopped too.
    pub other_processes: usize,
    /// The agent launches waiting to start in it that the stop removed from
    /// the agent queue.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cancelled: Vec<QueueEntry>,
    /// The run of the CI job in it the forge was asked to cancel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled_run: Option<String>,
}

/// The workspaces whose agents and commands a stop stopped, and the chats
/// it stopped, by name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoppedSpaces {
    pub stopped: Vec<SpaceName>,
    pub failed: Vec<Failure>,
    /// The agent launches waiting to start in them that the stop removed
    /// from the agent queue.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cancelled: Vec<QueueEntry>,
    /// The CI jobs' workspaces whose runs the forge was asked to cancel,
    /// which run until their runners stop them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cancelled_runs: Vec<CancelledRun>,
}

/// A CI job's workspace and the run the forge was asked to cancel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelledRun {
    pub workspace: SpaceName,
    pub run: String,
}

/// The processes Shoal does not track that a stop stopped, by space.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoppedProcesses {
    pub stopped: Vec<Stopped>,
    pub failed: Vec<Failure>,
}

/// The agents whose turns were interrupted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Interrupted {
    pub interrupted: Vec<Agent>,
    pub failed: Vec<Failure>,
}

/// The agents whose interrupted turns were continued.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Continued {
    pub continued: Vec<Agent>,
    pub failed: Vec<Failure>,
}

/// The agents a restart restarted, those whose restart the daemon took for
/// later, and those it left out.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Restarted {
    pub restarted: Vec<RestartedAgent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending: Vec<PendingAgent>,
    pub failed: Vec<Failure>,
    pub skipped: Vec<Skipped>,
    /// Whether the command was interrupted before the daemon answered for
    /// every agent, which it restarts all the same.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub interrupted: bool,
}

/// An agent a restart restarted, by the name of its space, a workspace or a
/// chat, and its own when known.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestartedAgent {
    pub space: SpaceName,
    pub agent: Option<shoal_protocol::agent::Agent>,
    /// Where it runs now, when its wrapper runs in a terminal host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place: Option<shoal_protocol::sessions::Origin>,
}

/// The spaces whose recorded restarts a cancel dropped, and those that had
/// none.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelledRestarts {
    pub cancelled: Vec<SpaceName>,
    pub unrecorded: Vec<SpaceName>,
}

/// An agent whose restart the daemon took for later, by its space's name,
/// with what the restart waits for.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingAgent {
    pub space: SpaceName,
    pub agent: shoal_protocol::agent::Agent,
    pub waits: shoal_protocol::model::RestartWait,
}

/// An agent a restart leaves out, by its space's name, and why.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skipped {
    pub space: SpaceName,
    pub agent: Option<shoal_protocol::agent::Agent>,
    pub reason: String,
}

outputs!(
    SpaceStopped,
    StoppedSpaces,
    StoppedProcesses,
    Interrupted,
    Continued,
    Restarted,
    CancelledRestarts
);

/// An agent session started detached in an app.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetachedSession {
    pub workspace: shoal_protocol::model::Workspace,
    pub agent: shoal_protocol::agent::Agent,
    pub execution_id: ExecutionId,
    pub pid: u32,
    pub log: std::path::PathBuf,
    #[serde(flatten)]
    pub delivery: Delivery,
}

outputs!(DetachedSession);
