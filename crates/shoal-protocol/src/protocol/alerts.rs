use crate::space_names::SpaceSelector;
use serde::{Deserialize, Serialize};

use super::Lifetime;

/// Requests about alerts: one-way notices to the user or a space's agent.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertMethod {
    /// The alerts for `to`, oldest first: the oldest unread ones when
    /// `unread_only`, so a backlog is shown in order across calls, otherwise
    /// the newest `limit`.
    ListAlerts {
        to: AlertsFor,
        unread_only: bool,
        limit: u32,
    },
    /// Mark exactly these alerts read, each one for the caller's space or,
    /// as the caller may read them, for the user.
    MarkAlertsRead { ids: Vec<i64> },
    /// Deletes every alert to the user, read or not; answers with how many.
    ClearAlerts,
    /// Long-lived: the user's unread alerts after `after`, then new ones as
    /// they are recorded, each as a [`super::Body::Alert`] response and
    /// marked read on delivery unless `keep_unread`.
    WatchAlerts { keep_unread: bool, after: i64 },
    /// An alert from the caller, answered with where it went.
    SendAlert {
        to: AlertTarget,
        /// The space the user acts as, as a session in it that Shoal did not
        /// start does; a scoped caller always acts as its own.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<SpaceSelector>,
        message: String,
        /// Files to send with it by absolute path, which the daemon copies.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<crate::files::NewFile>,
    },
    /// Delete the unread alert `id` the caller sent.
    WithdrawAlert {
        id: i64,
        /// The space the user acts as, as a session in it that Shoal did not
        /// start does; a scoped caller always acts as its own.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<SpaceSelector>,
    },
}

impl AlertMethod {
    pub fn lifetime(&self) -> Lifetime {
        match self {
            Self::WatchAlerts { .. } => Lifetime::Open,
            _ => Lifetime::Reply,
        }
    }
}

/// Whose alerts a listing reads.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertsFor {
    /// The user's.
    User,
    /// Those for the agent of a space.
    Space { space: SpaceSelector },
    /// The user's, as the caller may read them, and those of every space the
    /// caller's scope reaches.
    Every,
}

/// Where an alert goes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertTarget {
    /// Up from the caller's space: to its coordinator, the agent of the
    /// nearest space above with an agent, or else the user.
    Up,
    /// Down to the agent of a space the caller's scope reaches.
    Space { space: SpaceSelector },
    /// Down to every agent the caller's scope reaches but its own, of one
    /// repository when it names one.
    All {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repository: Option<crate::repository::Selector>,
    },
}
