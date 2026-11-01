//! What commands that report on every workspace print, such as `shoal
//! leases` without a workspace.
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{client::output::Output, model::Workspace};

/// One workspace's report, or why it could not be made.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
pub enum WorkspaceOverviewResult<T> {
    Ready(T),
    Failed {
        workspace: Box<Workspace>,
        error: String,
    },
}

impl<T: Output + DeserializeOwned> Output for WorkspaceOverviewResult<T> {
    type Owned = Self;
}
