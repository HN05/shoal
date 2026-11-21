//! What commands that act on any space, a workspace or a chat, print.
use shoal_protocol::space_names::SpaceName;

use serde::{Deserialize, Serialize};

use crate::output::outputs;
use shoal_protocol::{
    hub::directory::Listed,
    model::{Completion, Space},
};

/// A space `shoal list` prints: this machine's, or another joined
/// machine's as that machine last reported it, which names it.
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
#[allow(clippy::large_enum_variant, reason = "a listing holds a few spaces")]
pub enum ListedSpace {
    Elsewhere(Listed),
    Here(Space),
}

/// What recording a workspace's work as done says it did.
pub fn workspace_done(completion: &Completion) -> String {
    let marked = if completion.integrated {
        "Assignment marked done; integrated elsewhere"
    } else {
        "Assignment marked done"
    };
    if completion.cleanup {
        format!(
            "{marked}; cleanup requested. Tracked commands may stop and the workspace may be removed."
        )
    } else {
        format!("{marked}; no cleanup requested.")
    }
}

/// A recorded completion, withdrawn.
#[derive(Debug, Serialize, Deserialize)]
pub struct Withdrawn {
    pub withdrawn: Completion,
}

/// Whether an exported environment's scope was revoked.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Revoked {
    pub revoked: bool,
}

/// A start handed to a place beside the caller's terminal, which opens the
/// workspace and starts its agent there.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartedBeside {
    /// The name of the space whose agent or shell starts beside.
    pub space: SpaceName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<shoal_protocol::agent::Agent>,
}

outputs!(ListedSpace, Withdrawn, Revoked, StartedBeside);
