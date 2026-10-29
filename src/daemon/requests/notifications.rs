//! The daemon's answers to [`NotificationMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;
use tokio::net::UnixStream;

use super::super::{
    answer::{Answer, OperationGuard},
    scope::{Caller, Scoped, denied},
    workspace::Manager,
};
use crate::protocol::{self, Body, NotificationMethod, Response};

pub(super) async fn handle(manager: &Arc<Manager>, method: NotificationMethod) -> Result<Answer> {
    Ok(Answer::Body(match method {
        NotificationMethod::WatchNotifications { keep_unread, after } => {
            let manager = manager.clone();
            return Ok(Answer::connection(move |stream, request_id, guard| {
                watch_notifications(stream, request_id, manager, keep_unread, after, guard)
            }));
        }
        NotificationMethod::ListNotifications { unread_only, limit } => {
            // A listing of the most notifications a client asks for need not
            // fit one frame.
            let listed = manager.notifications(unread_only, limit).await;
            return Ok(Answer::pages(listed, Body::Notifications));
        }
        NotificationMethod::SendMessage { workspace, message } => {
            Body::Sent(manager.send_message(&workspace, message).await?)
        }
        NotificationMethod::WithdrawMessage { workspace, id } => {
            Body::Notification(manager.withdraw_message(&workspace, id).await?)
        }
        NotificationMethod::MarkNotificationsRead { ids } => {
            manager.mark_notifications_read(ids).await?;
            Body::Ok
        }
        NotificationMethod::ClearNotifications => {
            Body::Cleared(manager.clear_notifications().await?)
        }
    }))
}

impl Scoped for NotificationMethod {
    async fn scope(&mut self, _: &Manager, _: &Caller) -> Result<Option<&mut String>> {
        Ok(match self {
            NotificationMethod::SendMessage { workspace, .. }
            | NotificationMethod::WithdrawMessage { workspace, .. } => Some(workspace),
            NotificationMethod::ListNotifications { .. }
            | NotificationMethod::MarkNotificationsRead { .. }
            | NotificationMethod::ClearNotifications
            | NotificationMethod::WatchNotifications { .. } => return Err(denied()),
        })
    }
}

/// Long-lived notification stream: the unread backlog after `after`, then
/// each new notification as it is recorded, until the client hangs up.
async fn watch_notifications(
    mut stream: UnixStream,
    request_id: u64,
    manager: Arc<Manager>,
    keep_unread: bool,
    after: i64,
    startup: OperationGuard,
) -> Result<()> {
    let mut changed = manager.signals.watch_notifications();
    protocol::write(&mut stream, &Response::new(request_id, Body::Ok)).await?;
    drop(startup);
    let (mut reader, mut writer) = stream.split();
    let mut delivered = after;
    loop {
        let operation = manager.gates.operation().await;
        let batch = manager.notifications_after(delivered, 100).await?;
        for notification in batch {
            delivered = notification.id;
            let response = Response::new(request_id, Body::Notification(notification));
            protocol::write(&mut writer, &response).await?;
            if !keep_unread {
                manager.mark_notifications_read(vec![delivered]).await?;
            }
        }
        drop(operation);
        let mut closed = [0u8; 1];
        tokio::select! {
            recorded = changed.changed() => recorded?,
            // The client never writes; a read completes only when it hangs up.
            _ = tokio::io::AsyncReadExt::read(&mut reader, &mut closed) => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests;
