//! What `shoal status` prints, of this machine's space or another's.
use serde::{Deserialize, Serialize};
use shoal_protocol::sessions::TerminalSession;

use crate::output::outputs;
use shoal_protocol::{status::SpaceStatus, transcripts::Transcript};

/// The daemon's space status with the terminal sessions the CLI finds.
#[derive(Debug, Serialize, Deserialize)]
pub struct StatusView {
    #[serde(flatten)]
    pub status: SpaceStatus,
    pub sessions: Vec<TerminalSession>,
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

outputs!(StatusView);
