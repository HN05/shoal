use crate::space_ids::SpaceId;
use crate::space_names::SpaceSelector;
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
    /// A message from a space's agent to the user.
    SendMessage {
        space: SpaceSelector,
        message: String,
        /// Files to send with it by absolute path, which the daemon copies.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<crate::files::NewFile>,
        /// The chat whose agent sends the message, which attaches its files.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sender_chat: Option<SpaceId>,
    },
    /// Delete the unread message `id` the space sent the user.
    WithdrawMessage {
        space: SpaceSelector,
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
