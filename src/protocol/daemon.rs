use serde::{Deserialize, Serialize};

/// Daemon administration, which must reach a daemon of any release.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonMethod {
    Status,
    ReloadConfig,
}

/// Ask the daemon to exit. Daemons answer it before their protocol check, so
/// service commands reach a daemon of another release; keep its spelling.
#[derive(Debug, Serialize, Deserialize)]
pub struct ExitRequest {
    pub exit: Exit,
}

/// What happens to running work as the daemon exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Exit {
    /// Work stops as `shoal stop --all` stops it.
    Stop,
    /// Work keeps running and reattaches to the next daemon.
    Restart,
}
