//! Processes as Shoal records them: their identity, the untracked processes of
//! a space a stop takes, and the leftovers still running after what started
//! them exited.
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identity {
    pub pid: u32,
    pub birth: String,
    /// The kernel's never-reused process ID where the platform reports one,
    /// so the process's descendants stay attributable after it exits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unique: Option<UniqueId>,
}

/// PID and birth name the process; records made before `unique` existed
/// lack it.
impl PartialEq for Identity {
    fn eq(&self, other: &Self) -> bool {
        self.pid == other.pid && self.birth == other.birth
    }
}

impl Eq for Identity {}

impl std::hash::Hash for Identity {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.pid.hash(state);
        self.birth.hash(state);
    }
}

/// Unique process IDs start over at each boot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UniqueId {
    pub boot: String,
    pub id: u64,
}

/// Which of a space's untracked processes an operation takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Selection {
    /// Every untracked process, leftovers included.
    Processes,
    /// Only the processes left behind by what started them.
    Leftovers,
}

/// What a stop of a space's processes stopped.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stopped {
    /// The name of the workspace or chat.
    pub space: String,
    /// How many processes it stopped.
    pub processes: usize,
}

/// A leftover as people recognize it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Leftover {
    pub pid: u32,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<PathBuf>,
    /// When it started.
    pub started_at: crate::time::UnixTime,
}
