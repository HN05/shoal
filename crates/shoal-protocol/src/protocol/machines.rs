use serde::{Deserialize, Serialize};

use super::{Forwarding, Target};

/// Requests about a machine as a whole: this one, or the other joined
/// machine they name by its name or ID, which the daemon asks through the
/// hub.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MachineMethod {
    /// Pause the machine: it admits no new work and hands what it has over,
    /// until unpaused. Answers with [`super::Body::MachinePaused`].
    PauseMachine {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        machine: Option<String>,
    },
    /// Unpause the machine, which admits work again. Answers with
    /// [`super::Body::MachineUnpaused`].
    UnpauseMachine {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        machine: Option<String>,
    },
}

impl MachineMethod {
    /// Each acts on a machine, not a space, and names the machine it
    /// goes to.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::PauseMachine { .. } | Self::UnpauseMachine { .. } => Forwarding::Never,
        }
    }

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::PauseMachine { .. } | Self::UnpauseMachine { .. } => None,
        }
    }
}
