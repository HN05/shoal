//! The daemon's status and the file access stalls it reports.
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub pid: u32,
    pub version: String,
    pub uptime_secs: u64,
    pub managed: bool,
    pub unread_notifications: u64,
    /// Directories whose file access stalls now.
    #[serde(default)]
    pub access_stalls: Vec<AccessStall>,
    /// Why automatic cleanup is failing other than in a workspace, or has
    /// stalled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup_error: Option<String>,
}

/// A directory whose file access stalled, as while macOS waits for an answer
/// to a privacy prompt, until a later check there finishes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessStall {
    /// The directory holding the checked path, whose other paths' checks
    /// skip it meanwhile.
    pub directory: std::path::PathBuf,
    /// The path whose check stalled first.
    pub path: std::path::PathBuf,
    pub since: crate::time::UnixTime,
    /// How the check stalled, such as `did not finish within 30s`.
    pub reason: String,
}

impl AccessStall {
    /// What the user reads about the stall.
    pub fn message(&self) -> String {
        format!(
            "File access in {} stalled: a check of {} {}. A macOS privacy prompt may be waiting for an answer.",
            self.directory.display(),
            self.path.display(),
            self.reason
        )
    }
}
