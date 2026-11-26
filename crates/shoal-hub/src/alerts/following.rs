//! How each machine follows the other machines' alerts to the user.
use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use shoal_ids::member_ids::MemberId;
use shoal_protocol::hub::{alerts::AlertCopies, link::Call};
use tokio::sync::broadcast::error::RecvError;

use super::copies;
use crate::{
    Hub,
    daemon::Daemon,
    log,
    reports::RETRY,
    session::Calls,
    streams::{Incoming, Outgoing},
};

impl Hub {
    /// Follow the other machines' alerts over this machine's link for as
    /// long as it stays open, again once [`RETRY`] passes after the stream
    /// ends.
    pub(crate) async fn keep_following_alerts(&self, calls: &Calls) -> std::convert::Infallible {
        loop {
            if let Err(error) = self.follow_alerts(calls).await {
                log!("hub: could not follow the other machines' alerts: {error:#}");
            }
            tokio::time::sleep(RETRY).await;
        }
    }

    /// Follow the other machines' alerts on a stream from the hub until it
    /// ends.
    async fn follow_alerts(&self, calls: &Calls) -> Result<()> {
        let incoming = calls.stream(|stream| Call::FollowAlerts { stream }).await?;
        let mut lines = Lines::new(incoming);
        while let Some(copies) = lines.next().await? {
            self.notifications.take_copies(copies).await?;
        }
        bail!("the hub ended the stream of the other machines' alerts")
    }

    /// As the hub: write the other machines' alerts on `out`, the stream
    /// `member` follows them on, every one at once and then each change,
    /// until it stops reading or falls too far behind, which it opens the
    /// stream again for.
    pub(crate) fn serve_alert_copies(self: &Arc<Self>, member: MemberId, mut out: Outgoing) {
        let hub = self.clone();
        tokio::spawn(async move {
            let served = async {
                let mut changes = hub.alert_changes.follow();
                let except = member.clone();
                let whole = (hub.store)
                    .read(move |db| copies::others(db, &except))
                    .await?;
                write_line(&mut out, &AlertCopies::Whole(whole)).await?;
                loop {
                    let change = tokio::select! {
                        change = changes.recv() => change?,
                        () = out.closed() => return Ok(()),
                    };
                    if !copies::about(&change, &member) {
                        write_line(&mut out, &change).await?;
                    }
                }
            };
            let served: Result<()> = served.await;
            out.end(served.err().map(|error| format!("{error:#}")))
                .await;
        });
    }

    /// As the hub: keep the other machines' alerts in its own machine's
    /// notifications while this daemon takes the hub role, forgetting them
    /// once it ends.
    pub(crate) async fn keep_own_alert_copies(&self, daemon: &dyn Daemon) {
        let mut reloaded = self.config.watch();
        loop {
            if self.is_hub(daemon) {
                tokio::select! {
                    followed = self.follow_own_copies() => {
                        if let Err(error) = followed {
                            log!("hub: could not follow the other machines' alerts here: {error:#}");
                        }
                        tokio::time::sleep(RETRY).await;
                        continue;
                    }
                    () = self.hub_role_ended(daemon) => self.notifications.forget_copies(),
                }
            } else if reloaded.changed().await.is_err() {
                return;
            }
            reloaded.borrow_and_update();
        }
    }

    /// Take the other machines' copies into this machine's notifications,
    /// then each change, until it falls too far behind.
    async fn follow_own_copies(&self) -> Result<()> {
        let own = self.own_member().await?;
        let mut changes = self.alert_changes.follow();
        let except = own.clone();
        let whole = (self.store)
            .read(move |db| copies::others(db, &except))
            .await?;
        self.notifications
            .take_copies(AlertCopies::Whole(whole))
            .await?;
        loop {
            let change = match changes.recv().await {
                Ok(change) => change,
                Err(RecvError::Lagged(_)) => bail!("fell behind the changes to the copies"),
                Err(RecvError::Closed) => return Ok(()),
            };
            if !copies::about(&change, &own) {
                self.notifications.take_copies(change).await?;
            }
        }
    }
}

async fn write_line(out: &mut Outgoing, copies: &AlertCopies) -> Result<()> {
    let mut line = serde_json::to_vec(copies)?;
    line.push(b'\n');
    out.write(&line).await
}

/// The JSON lines a stream carries, as they come.
struct Lines {
    incoming: Incoming,
    buffer: Vec<u8>,
}

impl Lines {
    fn new(incoming: Incoming) -> Self {
        Self {
            incoming,
            buffer: Vec::new(),
        }
    }

    /// The next line's value, none once the stream ends.
    async fn next(&mut self) -> Result<Option<AlertCopies>> {
        loop {
            if let Some(end) = self.buffer.iter().position(|byte| *byte == b'\n') {
                let line: Vec<u8> = self.buffer.drain(..=end).collect();
                return Ok(Some(serde_json::from_slice(&line)?));
            }
            match self.incoming.next().await? {
                Some(bytes) => self.buffer.extend(bytes),
                None if self.buffer.is_empty() => return Ok(None),
                None => return Err(anyhow!("the stream ended inside a line")),
            }
        }
    }
}
