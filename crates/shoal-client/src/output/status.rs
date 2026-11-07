//! What `shoal work status` prints.
use serde::{Deserialize, Serialize};

use crate::{model::SpaceStatus, output::outputs, transcripts::Transcript};

/// The daemon's space status with the agent sessions the CLI finds.
#[derive(Debug, Serialize, Deserialize)]
pub struct StatusView {
    #[serde(flatten)]
    pub status: SpaceStatus,
    pub sessions: Vec<AgentSession>,
    /// Why sessions could not be listed; status still reports the rest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sessions_error: Option<String>,
    /// Every session the space's agents reported, with its files here.
    #[serde(default)]
    pub transcripts: Vec<Transcript>,
    /// Why transcripts could not be listed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcripts_error: Option<String>,
}

/// A running agent session that `shoal attach` can reach.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSession {
    pub name: String,
    /// The agent the daemon recorded running there, when one runs there.
    pub agent: Option<String>,
    /// Terminals attached to the session; a host that cannot count them
    /// gives one while it shows the session.
    pub clients: u32,
}

impl AgentSession {
    /// The name, agent, and whether a terminal is attached.
    pub fn label(&self) -> String {
        let mut label = self.name.clone();
        if let Some(agent) = &self.agent {
            label.push_str(&format!("  {agent}"));
        }
        if self.clients > 0 {
            label.push_str("  attached");
        }
        label
    }
}

outputs!(StatusView);
