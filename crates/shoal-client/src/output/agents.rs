//! What commands that act on running agents print: `shoal stop`, `shoal
//! restart` and the turn commands.
use crate::execution_ids::ExecutionId;
use crate::space_names::SpaceName;
use serde::{Deserialize, Serialize};

use crate::{
    model::QueueEntry,
    output::{batch::Failure, outputs},
    processes::Stopped,
};

/// An agent, by its workspace's name and its own, which is unknown for an
/// execution whose wrapper is gone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    pub workspace: SpaceName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<crate::agent::Agent>,
}

/// A stopped workspace's processes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceStopped {
    /// Whether its processes stopped; false for a CI job's workspace, whose
    /// run the forge was asked to cancel instead.
    pub stopped: bool,
    /// Whether agents or commands wait for `shoal resume`.
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

/// The workspaces whose agents and commands a stop stopped, by name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoppedWorkspaces {
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

/// The agents a restart restarted, and those it left out.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Restarted {
    pub restarted: Vec<RestartedAgent>,
    pub failed: Vec<Failure>,
    pub skipped: Vec<Skipped>,
    /// Whether the run was interrupted before every restart ended.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub interrupted: bool,
    /// The spaces whose restart goes on without an interrupted run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending: Vec<SpaceName>,
}

/// An agent a restart restarted, by the name of its space, a workspace or a
/// chat, and its own when known.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestartedAgent {
    pub space: SpaceName,
    pub agent: Option<crate::agent::Agent>,
    /// Where it runs now, when its wrapper runs in a terminal host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place: Option<crate::sessions::Origin>,
}

/// An agent a restart leaves out, by its space's name, and why.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skipped {
    pub space: SpaceName,
    pub agent: Option<crate::agent::Agent>,
    pub reason: String,
}

outputs!(
    WorkspaceStopped,
    StoppedWorkspaces,
    StoppedProcesses,
    Interrupted,
    Continued,
    Restarted
);

/// What an app's session did with an agent's first message, which its
/// detached launch reports.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Delivery {
    /// Where the first message waits for the user to send it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_file: Option<std::path::PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_delivered: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub happy_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub happy_daemon_recorded: Option<bool>,
}

/// An agent session started detached in an app.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetachedSession {
    pub workspace: crate::model::Workspace,
    pub agent: crate::agent::Agent,
    pub execution_id: ExecutionId,
    pub pid: u32,
    pub log: std::path::PathBuf,
    #[serde(flatten)]
    pub delivery: Delivery,
}

/// A chat started detached in an app.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetachedChat {
    pub chat: crate::model::Chat,
    pub log: std::path::PathBuf,
    #[serde(flatten)]
    pub delivery: Delivery,
}

outputs!(DetachedSession, DetachedChat);
