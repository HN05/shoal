//! Alerts: one-way notices to the user or a space's agent, from the user, a
//! space or Shoal.
use shoal_protocol::alerts::{Alert, Cleared, Sent};

use anyhow::{Context, Result};

use tokio::{io::BufReader, net::UnixStream};

use crate::connection::{Daemon, open_buffered, pages, reconnect, request, stream_closed};
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::{
    chat_marks::ChatMarked,
    files::NewFile,
    protocol::{self, AlertMethod, AlertTarget, AlertsFor, Response},
};

/// The alerts for `to`: the oldest unread ones when `unread_only`, else the
/// newest `limit`, oldest first.
pub async fn list(
    daemon: &impl Daemon,
    to: AlertsFor,
    unread_only: bool,
    limit: u32,
) -> Result<Vec<Alert>> {
    let method = AlertMethod::ListAlerts {
        to,
        unread_only,
        limit,
    };
    pages(daemon, method).await
}

/// Mark the alerts `ids` read, so they do not come back as new.
pub async fn mark_read(daemon: &impl Daemon, ids: Vec<i64>) -> Result<()> {
    request(daemon, AlertMethod::MarkAlertsRead { ids }).await
}

/// Delete every alert to the user.
pub async fn clear(daemon: &impl Daemon) -> Result<Cleared> {
    request(daemon, AlertMethod::ClearAlerts).await
}

/// Send `message` with `attachments` to `to`, as the space `from` names
/// when the user acts as one.
pub async fn send(
    daemon: &impl Daemon,
    to: AlertTarget,
    from: Option<SpaceSelector>,
    message: String,
    attachments: Vec<NewFile>,
) -> Result<Sent> {
    let method = AlertMethod::SendAlert {
        to,
        from,
        message,
        attachments,
    };
    request(daemon, method).await
}

/// Mark the calling agent's turn as sending its final message to `to`
/// with `attachments` once it ends, with the mark it replaced.
pub async fn send_in_chat(
    daemon: &impl Daemon,
    to: AlertTarget,
    attachments: Vec<NewFile>,
) -> Result<ChatMarked> {
    request(daemon, AlertMethod::AlertInChat { to, attachments }).await
}

/// Take back the unread alert `id` the caller sent, or the space `from`
/// names when the user acts as one.
pub async fn withdraw(
    daemon: &impl Daemon,
    id: i64,
    space: Option<SpaceSelector>,
    from: Option<SpaceSelector>,
) -> Result<Alert> {
    request(daemon, AlertMethod::WithdrawAlert { id, space, from }).await
}

/// The user's alerts the daemon records, as it records them, after the one
/// last delivered, across daemon restarts. The daemon marks each read as it
/// delivers it unless the watch keeps them unread.
pub struct Watch {
    stream: BufReader<UnixStream>,
    keep_unread: bool,
    delivered: i64,
}

impl Watch {
    /// Watch the alerts after `after`.
    pub async fn start(daemon: &impl Daemon, keep_unread: bool, after: i64) -> Result<Self> {
        let (stream, body) = open_buffered(daemon, watch(keep_unread, after)).await?;
        <()>::try_from(body)?;
        Ok(Self {
            stream,
            keep_unread,
            delivered: after,
        })
    }

    /// The ID of the last alert delivered, which a later watch
    /// resumes after.
    pub fn delivered(&self) -> i64 {
        self.delivered
    }

    /// The next alert, once the daemon records it.
    pub async fn next(&mut self, daemon: &impl Daemon) -> Result<Alert> {
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
                Err(error) => return Err(error).context("daemon closed the alert stream"),
            };
            let alert = Alert::try_from(response.body)?;
            self.delivered = alert.id;
            return Ok(alert);
        }
    }
}

fn watch(keep_unread: bool, after: i64) -> AlertMethod {
    AlertMethod::WatchAlerts { keep_unread, after }
}
