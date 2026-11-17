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
    /// The hub role the daemon takes, none when it neither is the hub nor
    /// joined one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hub: Option<crate::hub::HubRole>,
}

/// The cause besides a slow system that file access timeouts name: a privacy
/// prompt, which only macOS holds file access for.
pub const PRIVACY_PROMPT: Option<&str> = if cfg!(target_os = "macos") {
    Some("a macOS privacy prompt may be waiting for an answer")
} else {
    None
};

/// A directory whose file access timed out, as under heavy memory pressure or
/// while macOS waits for an answer to a privacy prompt, until a later check
/// there finishes.
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
        let mut message = format!(
            "File access in {} timed out: a check of {} {}. Shoal checks again",
            self.directory.display(),
            self.path.display(),
            self.reason
        );
        if let Some(prompt) = PRIVACY_PROMPT {
            message.push_str(&format!("; if it keeps timing out, {prompt}"));
        }
        message.push('.');
        message
    }
}

#[cfg(test)]
mod tests;
