use serde::{Deserialize, Serialize};

/// Requests about workspaces' running demos.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DemoMethod {
    /// Running demos, of one workspace or, unscoped, of every workspace.
    ListDemos { workspace: Option<String> },
    /// How far the start of one demo launch in a workspace got.
    DemoProgress { workspace: String, launch: String },
    /// Ask a workspace's demos to stop: the one named, or every one.
    StopDemos {
        workspace: String,
        name: Option<String>,
    },
}
