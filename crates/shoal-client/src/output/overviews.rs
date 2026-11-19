//! What commands that report on every workspace print, such as `shoal
//! leases` without a workspace.
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::output::{Output, outputs};
use shoal_protocol::{model::Workspace, resources::Overview};

/// A workspace's resource leases.
#[derive(Debug, Serialize, Deserialize)]
pub struct WorkspaceOverview {
    pub workspace: Workspace,
    #[serde(flatten)]
    pub overview: Overview,
}

outputs!(WorkspaceOverview);

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
