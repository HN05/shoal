//! What `shoal daemon` prints.
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{output::outputs, protocol::DaemonStatus, sessions::HostName};

/// A service definition, as `shoal daemon install --dry-run` prints it.
#[derive(Serialize, Deserialize)]
pub struct Definition {
    pub path: PathBuf,
    pub content: String,
}

/// An installed daemon service, running.
#[derive(Debug, Serialize, Deserialize)]
pub struct Installed {
    pub running: bool,
    pub service_file: PathBuf,
    /// The global config.
    pub config: PathBuf,
    /// Whether the install wrote the global config.
    pub config_created: bool,
    /// The line that sets up shell integration.
    pub shell_init: String,
    pub installed_terminal_hosts: Vec<HostName>,
}

/// Whether a daemon runs, and its status when it answers.
#[derive(Debug, Serialize, Deserialize)]
pub struct Status {
    pub running: bool,
    pub daemon: Option<DaemonStatus>,
    pub socket: PathBuf,
}

/// A daemon started, or stopped with whether work it stopped waits for
/// `shoal resume`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Running {
    pub running: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_work: Option<bool>,
}

/// A daemon that reread the global config.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Reloaded {
    pub reloaded: bool,
}

/// A packaged config installed as the global config.
#[derive(Debug, Serialize, Deserialize)]
pub struct ConfigInstalled {
    pub name: String,
    pub config: PathBuf,
    /// The previous config, kept.
    pub backup: Option<PathBuf>,
    pub daemon_reloaded: bool,
}

outputs!(
    Definition,
    Installed,
    Status,
    Running,
    Reloaded,
    ConfigInstalled
);
