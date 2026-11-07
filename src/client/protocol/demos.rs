use crate::client::space_names::SpaceSelector;
use serde::{Deserialize, Serialize};

/// Requests about workspaces' running demos.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DemoMethod {
    /// Running demos, of one workspace or, unscoped, of every workspace.
    ListDemos { workspace: Option<SpaceSelector> },
    /// How far the start of one demo launch in a workspace got.
    DemoProgress {
        workspace: SpaceSelector,
        launch: String,
    },
    /// Ask a workspace's demos to stop: the one named, or every one.
    StopDemos {
        workspace: SpaceSelector,
        name: Option<String>,
    },
}
