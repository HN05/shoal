//! The daemon's answers to [`AlertMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;
use tokio::net::UnixStream;

use super::super::{
    alerts::users_alerts,
    answer::{Answer, OperationGuard},
    manager::Manager,
    scope::{Caller, Scoped},
};
use super::acting::acting;
use crate::protocol::{self, AlertMethod, AlertTarget, AlertsFor, Body, Response};
use shoal_protocol::space_names::SpaceSelector;

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: AlertMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        AlertMethod::WatchAlerts { keep_unread, after } => {
            let manager = manager.clone();
            return Ok(Answer::connection(move |stream, request_id, guard| {
                watch_alerts(stream, request_id, manager, keep_unread, after, guard)
            }));
        }
        AlertMethod::ListAlerts {
            to,
            unread_only,
            limit,
        } => {
            // A listing of the most alerts a client asks for need not fit
            // one frame.
            let listed = manager.alerts_for(caller, to, unread_only, limit).await;
            return Ok(Answer::pages(listed, Body::Alerts));
        }
        AlertMethod::SendAlert {
            to,
            from,
            message,
            attachments,
        } => {
            // A message the user sends an agent is a prompt they submit, to
            // each agent it reaches.
            let to_all = matches!(to, AlertTarget::All { .. });
            if let AlertTarget::Space { space } = &to {
                manager.record_user_interaction(caller, space).await;
            }
            let acting = acting(manager, caller, from).await?;
            let sent = (manager.send_alert(acting.as_ref(), to, message, attachments)).await?;
            if to_all {
                manager.record_users_message(caller, &sent.reached).await;
            }
            Body::Sent(sent)
        }
        AlertMethod::AlertInChat { to, attachments } => {
            Body::ChatMarked(manager.mark_chat_alert(caller, to, attachments).await?)
        }
        AlertMethod::WithdrawAlert { id, space, from } => {
            let acting = acting(manager, caller, from).await?;
            let withdrawn = manager.withdraw_alert(acting.as_ref(), space.as_ref(), id);
            Body::Alert(withdrawn.await?)
        }
        AlertMethod::MarkAlertsRead { ids } => {
            manager.mark_alerts_read(caller, ids).await?;
            Body::Ok
        }
        AlertMethod::ClearAlerts => Body::Cleared(manager.notifications.clear().await?),
    }))
}

/// Reading, clearing and watching the user's alerts is the `notifications`
/// permission's, and a scoped process sends alerts down only to the spaces
/// below its own; the handlers narrow the other requests to what the
/// caller's scope reaches.
impl Scoped for AlertMethod {
    async fn scope(
        &mut self,
        manager: &Manager,
        caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        // A scoped caller sends and withdraws as its own space.
        if let AlertMethod::SendAlert { from, .. } | AlertMethod::WithdrawAlert { from, .. } = self
        {
            *from = None;
        }
        Ok(match self {
            AlertMethod::ListAlerts {
                to: AlertsFor::Space { space },
                ..
            } => Some(space),
            AlertMethod::ListAlerts {
                to: AlertsFor::User,
                ..
            }
            | AlertMethod::ClearAlerts
            | AlertMethod::WatchAlerts { .. } => {
                manager
                    .authorize_action(Some(caller), users_alerts())
                    .await?;
                None
            }
            AlertMethod::SendAlert { to, .. } | AlertMethod::AlertInChat { to, .. } => {
                manager.scope_alert_target(caller, to).await?;
                None
            }
            AlertMethod::ListAlerts {
                to: AlertsFor::Every,
                ..
            }
            | AlertMethod::MarkAlertsRead { .. }
            | AlertMethod::WithdrawAlert { .. } => None,
        })
    }
}

/// Long-lived stream of the user's alerts: the unread backlog after
/// `after`, then each new one as it is recorded, until the client hangs up.
async fn watch_alerts(
    mut stream: UnixStream,
    request_id: u64,
    manager: Arc<Manager>,
    keep_unread: bool,
    after: i64,
    startup: OperationGuard,
) -> Result<()> {
    let mut changed = manager.notifications.watch();
    protocol::write(&mut stream, &Response::new(request_id, Body::Ok)).await?;
    drop(startup);
    let (mut reader, mut writer) = stream.split();
    let mut delivered = after;
    loop {
        let operation = manager.gates.operation().await;
        let batch = manager.notifications.after(delivered, 100).await?;
        for notification in batch {
            delivered = notification.id;
            let response = Response::new(request_id, Body::Alert(notification));
            protocol::write(&mut writer, &response).await?;
            if !keep_unread {
                manager.notifications.mark_read(vec![delivered]).await?;
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
