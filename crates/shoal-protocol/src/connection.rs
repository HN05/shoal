//! What a connection to the daemon can find that a caller reads without
//! one: a daemon that speaks another protocol version.
use crate::protocol;

/// The daemon answered with another protocol `version`.
#[derive(Debug)]
pub struct ProtocolMismatch {
    pub daemon: u32,
}

impl std::fmt::Display for ProtocolMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "daemon protocol mismatch: the daemon speaks protocol {}, this Shoal {}; run `shoal daemon install` to update the managed daemon, or restart a foreground daemon with the installed version",
            self.daemon,
            protocol::VERSION
        )
    }
}

impl std::error::Error for ProtocolMismatch {}
