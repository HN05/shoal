use serde::{Deserialize, Serialize};

use super::Lifetime;

/// Requests about notifications and the messages workspaces send the user.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationMethod {
    ListNotifications {
        unread_only: bool,
        limit: u32,
    },
    MarkNotificationsRead {
        ids: Vec<i64>,
    },
    /// Deletes every notification, read or not; answers with how many.
    ClearNotifications,
    /// Long-lived: unread notifications after `after`, then new ones as they
    /// are recorded, each as a [`Body::Notification`] response and marked
    /// read on delivery unless `keep_unread`.
    WatchNotifications {
        keep_unread: bool,
        after: i64,
    },
    /// A message from a workspace process to the user.
    SendMessage {
        workspace: String,
        message: String,
    },
    /// Delete the unread message `id` the workspace sent the user.
    WithdrawMessage {
        workspace: String,
        id: i64,
    },
}

impl NotificationMethod {
    pub fn lifetime(&self) -> Lifetime {
        match self {
            Self::WatchNotifications { .. } => Lifetime::Open,
            _ => Lifetime::Reply,
        }
    }
}
