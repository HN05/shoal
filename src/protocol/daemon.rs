use serde::{Deserialize, Serialize};

/// Daemon administration, which must reach a daemon of any release.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonMethod {
    Status,
    Shutdown,
    ReloadConfig,
}
