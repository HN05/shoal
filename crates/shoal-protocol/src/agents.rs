//! What the daemon gives an agent running in a space: the login its Git
//! sends, and what an app's session did with its first message.
use serde::{Deserialize, Serialize};

/// A login Git sends over HTTPS. Its `Debug` leaves out the password.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCredential {
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for GitCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GitCredential")
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}

/// What an app's session did with an agent's first message, which its
/// detached launch reports.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Delivery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub happy_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub happy_daemon_recorded: Option<bool>,
}
