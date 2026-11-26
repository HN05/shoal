//! Requests about alerts, the one-way notices to the user or a space's
//! agent.
use shoal_protocol::alerts::{Alert, AlertCursor, Cleared, Sent};

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use shoal_ids::member_ids::MemberId;

use tokio::{io::BufReader, net::UnixStream};

use crate::connection::{
    Daemon, Prepared, open_buffered, pages, reconnect, request, stream_closed,
};
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

/// Mark this machine's alerts `ids` read, so they do not come back as new.
pub async fn mark_read(daemon: &impl Daemon, ids: Vec<i64>) -> Result<()> {
    let elsewhere = Default::default();
    request(daemon, AlertMethod::MarkAlertsRead { ids, elsewhere }).await
}

/// Mark `shown` read where each is kept, this machine's or another's, so
/// they do not come back as new.
pub async fn mark_shown_read(daemon: &impl Daemon, shown: &[Alert]) -> Result<()> {
    let mut ids = Vec::new();
    let mut elsewhere: BTreeMap<MemberId, Vec<i64>> = BTreeMap::new();
    for alert in shown {
        match &alert.machine {
            None => ids.push(alert.id),
            Some(machine) => elsewhere
                .entry(machine.id.clone())
                .or_default()
                .push(alert.id),
        }
    }
    request(daemon, AlertMethod::MarkAlertsRead { ids, elsewhere }).await
}

/// Delete every alert to the user.
pub async fn clear(daemon: &impl Daemon) -> Result<Cleared> {
    request(daemon, AlertMethod::ClearAlerts).await
}

/// Send `message` with `attachments` to `to`, as the space `from` names
/// when the user acts as one.
pub fn send(
    to: AlertTarget,
    from: Option<SpaceSelector>,
    message: String,
    attachments: Vec<NewFile>,
) -> Prepared<Sent> {
    Prepared::new(AlertMethod::SendAlert {
        to,
        from,
        message,
        attachments,
    })
}

/// Mark the calling agent's turn as sending its final message to `to`
/// with `attachments` once it ends, with the mark it replaced.
pub fn send_in_chat(to: AlertTarget, attachments: Vec<NewFile>) -> Prepared<ChatMarked> {
    Prepared::new(AlertMethod::AlertInChat { to, attachments })
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

/// The user's alerts, this machine's and the other joined machines', as
/// they come, past where the watch stands, across daemon restarts. The
/// daemon marks each read as it delivers it unless the watch keeps them
/// unread.
pub struct Watch {
    stream: BufReader<UnixStream>,
    keep_unread: bool,
    delivered: AlertCursor,
}

impl Watch {
    /// Watch the alerts `after` has not passed.
    pub async fn start(
        daemon: &impl Daemon,
        keep_unread: bool,
        after: AlertCursor,
    ) -> Result<Self> {
        let (stream, body) = open_buffered(daemon, watch(keep_unread, after.clone())).await?;
        <()>::try_from(body)?;
        Ok(Self {
            stream,
            keep_unread,
            delivered: after,
        })
    }

    /// Where the watch stands, which a later watch resumes from.
    pub fn delivered(&self) -> &AlertCursor {
        &self.delivered
    }

    pub async fn next(&mut self, daemon: &impl Daemon) -> Result<Alert> {
        loop {
            let response: Response = match protocol::read_buffered(&mut self.stream).await {
                Ok(response) => response,
                Err(error) if stream_closed(&error) => {
                    let (keep_unread, after) = (self.keep_unread, self.delivered.clone());
                    let (stream, body) =
                        reconnect(daemon, || watch(keep_unread, after.clone())).await?;
                    <()>::try_from(body)?;
                    self.stream = stream;
                    continue;
                }
                Err(error) => return Err(error).context("daemon closed the alert stream"),
            };
            let alert = Alert::try_from(response.body)?;
            self.delivered.deliver(&alert);
            return Ok(alert);
        }
    }
}

fn watch(keep_unread: bool, after: AlertCursor) -> AlertMethod {
    AlertMethod::WatchAlerts { keep_unread, after }
}
