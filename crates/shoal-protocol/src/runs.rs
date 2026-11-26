//! The `shoal run` commands the daemon admits by their measured memory:
//! what tells one command's runs apart from another's, and what the
//! wrapper and the daemon say to each other about a run's admission.
use serde::{Deserialize, Serialize};

use crate::model::HeldBy;
use shoal_ids::execution_ids::ExecutionId;

/// What tells the runs of a `shoal run` command apart, whose peaks one
/// estimate reads: a configured command's name, whatever arguments follow
/// it, or the exact arguments of `shoal run -- …`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandKey {
    Named(String),
    Argv(Vec<String>),
}

impl std::fmt::Display for CommandKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Named(name) => f.write_str(name),
            Self::Argv(argv) => f.write_str(&argv.join(" ")),
        }
    }
}

/// How a command asks for admission.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmitAs {
    /// A new run, which waits until it fits.
    #[default]
    New,
    /// A run again after protection stopped it, which waits for healthy
    /// readings too.
    AfterStop,
    /// The run of `execution`, running already, whose wrapper lost the
    /// daemon that admitted it: admitted at once, only to that wrapper.
    Running { execution: ExecutionId },
}

/// Daemon → wrapper frames about a command's admission, after the
/// [`AdmitCommand`](crate::protocol::QueueMethod::AdmitCommand) response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AdmissionFrame {
    /// The command waits; `held` says for what at the last admission pass,
    /// none before one has weighed it.
    Waiting { held: Option<HeldBy> },
    /// The command may run, and holds its admission until it ends.
    Admitted,
    /// Protection stopped the command for `reason`: the wrapper stops it,
    /// then asks again to run it once it fits.
    Stop { reason: String },
}

/// Wrapper → daemon frames about an admitted command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunReport {
    /// The memory, in bytes, the running command holds now.
    Using { memory: u64 },
    /// The run ended: the most memory it held, how long it ran, whether it
    /// ran to its own end rather than being stopped or interrupted, and
    /// whether the kernel's OOM killer killed a process of it.
    Ran {
        peak: u64,
        duration_ms: u64,
        whole: bool,
        oom_killed: bool,
    },
}

#[cfg(test)]
mod tests;
