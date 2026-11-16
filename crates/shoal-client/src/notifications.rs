//! The user's notifications: what the daemon recorded for the user to read.
use shoal_protocol::notifications::{Cleared, Notification, Sent};

use anyhow::{Context, Result};

use shoal_ids::space_ids::SpaceId;

use tokio::{io::BufReader, net::UnixStream};

use crate::connection::{Daemon, open_buffered, pages, reconnect, request, stream_closed};
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::{
    files::NewFile,
    protocol::{self, NotificationMethod, Response},
};

/// The newest `limit` notifications, only the unread ones when `unread_only`.
pub async fn list(
    daemon: &impl Daemon,
    unread_only: bool,
    limit: u32,
) -> Result<Vec<Notification>> {
    pages(
        daemon,
        NotificationMethod::ListNotifications { unread_only, limit },
    )
    .await
}

pub async fn mark_read(daemon: &impl Daemon, ids: Vec<i64>) -> Result<()> {
    request(daemon, NotificationMethod::MarkNotificationsRead { ids }).await
}

/// Delete every notification.
pub async fn clear(daemon: &impl Daemon) -> Result<Cleared> {
    request(daemon, NotificationMethod::ClearNotifications).await
}

/// Send the user, or the coordinating agent, `message` from `space` with
/// `attachments`; the chat `sender_chat` names, when its agent sends it,
/// attaches its files.
pub async fn send(
    daemon: &impl Daemon,
    space: SpaceSelector,
    message: String,
    attachments: Vec<NewFile>,
    sender_chat: Option<SpaceId>,
) -> Result<Sent> {
    let method = NotificationMethod::SendMessage {
        space,
        message,
        attachments,
        sender_chat,
    };
    request(daemon, method).await
}

/// Take back `space`'s unread notification `id`.
pub async fn withdraw(daemon: &impl Daemon, space: SpaceSelector, id: i64) -> Result<Notification> {
    request(daemon, NotificationMethod::WithdrawMessage { space, id }).await
}

/// The notifications the daemon records, as it records them, after the one
/// last delivered, across daemon restarts. The daemon marks each read as it
/// delivers it unless the watch keeps them unread.
pub struct Watch {
    stream: BufReader<UnixStream>,
    keep_unread: bool,
    delivered: i64,
}

impl Watch {
    /// Watch the notifications after `after`.
    pub async fn start(daemon: &impl Daemon, keep_unread: bool, after: i64) -> Result<Self> {
        let (stream, body) = open_buffered(daemon, watch(keep_unread, after)).await?;
        <()>::try_from(body)?;
        Ok(Self {
            stream,
            keep_unread,
            delivered: after,
        })
    }

    /// The ID of the last notification delivered, which a later watch
    /// resumes after.
    pub fn delivered(&self) -> i64 {
        self.delivered
    }

    /// The next notification, once the daemon records it.
    pub async fn next(&mut self, daemon: &impl Daemon) -> Result<Notification> {
        loop {
            let response: Response = match protocol::read_buffered(&mut self.stream).await {
                Ok(response) => response,
                Err(error) if stream_closed(&error) => {
                    let (keep_unread, after) = (self.keep_unread, self.delivered);
                    let (stream, body) = reconnect(daemon, || watch(keep_unread, after)).await?;
                    <()>::try_from(body)?;
                    self.stream = stream;
                    continue;
                }
                Err(error) => return Err(error).context("daemon closed the notification stream"),
            };
            let notification = Notification::try_from(response.body)?;
            self.delivered = notification.id;
            return Ok(notification);
        }
    }
}

fn watch(keep_unread: bool, after: i64) -> NotificationMethod {
    NotificationMethod::WatchNotifications { keep_unread, after }
}
