//! What Shoal reports of a space to the terminal its agent runs in, through
//! the Program Status Protocol (OSC 7501): the daemon decides each report,
//! and the execution wrapper writes it to its terminal.
use serde::{Deserialize, Serialize};

/// A report of a space's status, which replaces the one before it whole.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgramStatus {
    #[serde(flatten)]
    pub state: ProgramState,
    /// The space's name, which labels the report.
    pub title: String,
    /// One line saying what the space waits for, works on, finished or
    /// failed at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// The state is the turn the agent's hooks report, which ends when the
    /// agent exits.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub turn: bool,
}

/// The protocol's states Shoal reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProgramState {
    Idle,
    Working {
        /// The percentage of its steps done, when the agent has steps.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        progress: Option<u8>,
    },
    /// The user must act.
    Blocked {
        kind: Waits,
    },
    Done,
    Error,
    /// Nothing to report: the record goes.
    Clear,
}

/// What a blocked space waits for the user to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Waits {
    /// Approve or deny something.
    Permission,
    /// Answer.
    Question,
}
