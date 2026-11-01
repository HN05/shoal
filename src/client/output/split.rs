//! What `shoal split` prints.
use serde::{Deserialize, Serialize};

use crate::{
    client::output::outputs,
    model::{QueueEntry, Workspace},
};

/// The ready child and the queue entry of its agent, when one was started.
#[derive(Serialize, Deserialize)]
pub struct Child {
    #[serde(flatten)]
    pub workspace: Workspace,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<QueueEntry>,
}

outputs!(Child);
