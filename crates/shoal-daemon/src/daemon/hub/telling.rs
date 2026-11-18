//! Events that go up a tree spanning machines: what a space passes to its
//! coordinator goes to the machine of the first space above it that runs
//! elsewhere, which hands it to the nearest space from there up with an
//! agent, passes it on to the next machine where the tree goes on, or
//! tells the user at the top. While a machine's link is down, what its
//! spaces pass up waits in its outbox and goes in order once the link
//! opens again; the hub holds what it cannot deliver yet.
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use rusqlite::{OptionalExtension as _, Transaction, params};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    hub::{
        link::Call,
        routing::{Expired, Told, ToldAbout},
    },
    notifications::NotificationKind,
    space_names::SpaceName,
};

use super::{membership, session::Calls};
use crate::{
    daemon::{
        agent_messages::About,
        log,
        workspace::{Manager, Receiver},
    },
    time::UnixTime,
};

/// An event a space passes up: the space it is about, the notification it
/// is for the user, its message and the files it sent.
pub(crate) struct Event<'a> {
    pub name: &'a SpaceName,
    pub space: &'a SpaceId,
    pub kind: Option<NotificationKind>,
    pub message: String,
    pub attachments: &'a [PathBuf],
}

impl Manager {
    /// Pass `event` up to the space `above`, which another machine runs.
    pub(crate) async fn pass_up(&self, above: SpaceId, event: Event<'_>) -> Result<()> {
        let files = (event.attachments.iter())
            .filter_map(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .collect();
        let told = Told {
            above,
            about: Some(ToldAbout {
                space: event.space.clone(),
                name: event.name.clone(),
            }),
            kind: event.kind,
            message: event.message,
            files,
        };
        self.send_told(told).await
    }

    /// Message the agent of the space `id`, which another machine runs.
    pub(crate) async fn message_elsewhere(&self, id: &SpaceId, message: String) -> Result<()> {
        let told = Told {
            above: id.clone(),
            about: None,
            kind: None,
            message,
            files: Vec::new(),
        };
        self.send_told(told).await
    }

    /// Send `told` through the hub: this daemon's own routing on the hub,
    /// else this machine's link, or its outbox while the link is down.
    pub(super) async fn send_told(&self, told: Told) -> Result<()> {
        if self.is_hub() {
            return (self.route_told(&self.own_member().await?, told).await).map(drop);
        }
        if self.store.read(membership::load).await?.is_none() {
            anyhow::bail!("this machine joined no hub to pass the event to");
        }
        let call = Call::Tell { told };
        if let Some(calls) = self.hub.member.calls()
            && calls.call(call.clone()).await.is_ok()
        {
            return Ok(());
        }
        let queued_at = UnixTime::now();
        (self.store)
            .write(move |tx| push_outbox(tx, &call, queued_at))
            .await
    }

    /// Send what waited in the outbox for the link `calls` serves, oldest
    /// first, until none is left or a call fails, which keeps it for the
    /// next link.
    pub(super) async fn send_outbox(&self, calls: &Calls) {
        loop {
            let next = match self.store.read(first_outbox).await {
                Ok(Some(next)) => next,
                Ok(None) => return,
                Err(error) => return log!("hub: could not read the outbox: {error:#}"),
            };
            let (seq, call) = next;
            match calls.call(call).await {
                Ok(_) => {}
                // The hub answered, but could not deliver it, as for a space
                // it no longer lists.
                Err(error) if self.hub.member.calls().is_some() => {
                    log!("hub: an event in the outbox could not be passed up: {error:#}")
                }
                Err(_) => return,
            }
            if let Err(error) = self.store.write(move |tx| remove_outbox(tx, seq)).await {
                return log!("hub: could not drop a sent event: {error:#}");
            }
        }
    }

    /// Deliver `told` to the nearest space from `told.above` up with an
    /// agent, pass it on to the machine where the tree goes on, or tell
    /// the user what is for the user.
    pub(super) async fn deliver_told(&self, told: Told) -> Result<()> {
        let agents = self.tracked_agents().await;
        let above = told.above.clone();
        let receiver = (self.store)
            .run(move |db| crate::daemon::workspace::receiver_from(db, &above, &agents))
            .await?;
        let message = with_files(&told);
        match (receiver, told.about) {
            (Some(Receiver::Here(coordinator)), about) => {
                let about = about.map(|about| About {
                    name: about.name,
                    id: Some(about.space),
                    kind: told.kind,
                });
                (self.queue_space_message(&coordinator, about, message, Vec::new())).await
            }
            (Some(Receiver::Elsewhere(next)), about) => {
                let told = Told {
                    above: next,
                    about,
                    ..told
                };
                self.send_told(told).await
            }
            (None, Some(about)) => {
                if let Some(kind) = told.kind {
                    (self.notify_user_of_space(&about.name, &about.space, kind, message)).await;
                }
                Ok(())
            }
            (None, None) => anyhow::bail!("space {} has no agent to tell", told.above),
        }
    }

    /// Tell whoever sent what expired at the hub: the space that sent it,
    /// when it still has an agent here, else the user.
    pub(super) async fn hear_expired(&self, expired: Expired) {
        let (summary, machine) = (&expired.summary, &expired.machine);
        let queued = expired.queued_at.local_minutes();
        let message = match expired.unconfirmed {
            false => format!(
                "The hub dropped {summary}, which waited for machine {machine} from {queued} and expired before that machine connected"
            ),
            true => format!(
                "The hub delivered {summary}, which waited for machine {machine} from {queued}, but the link closed before {machine} confirmed it ran; check there before sending it again"
            ),
        };
        if let Some(space) = &expired.from
            && (self.queue_agent_message(space, message.clone()).await).is_ok()
        {
            return;
        }
        self.notify(None, NotificationKind::HubExpired, message)
            .await;
    }
}

/// `told`'s message, naming the files it sent, which stay on its machine.
fn with_files(told: &Told) -> String {
    match told.files.is_empty() {
        true => told.message.clone(),
        false => format!(
            "{}\nFiles it sent, which stay on its machine: {}",
            told.message,
            told.files.join(", ")
        ),
    }
}

fn push_outbox(tx: &Transaction<'_>, call: &Call, queued_at: UnixTime) -> Result<()> {
    tx.execute(
        "INSERT INTO hub_outbox (call, queued_at) VALUES (?1, ?2)",
        params![serde_json::to_string(call)?, queued_at],
    )?;
    Ok(())
}

fn first_outbox(tx: &Transaction<'_>) -> Result<Option<(i64, Call)>> {
    let row = tx
        .query_row(
            "SELECT seq, call FROM hub_outbox ORDER BY seq LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    row.map(|(seq, call)| {
        Ok((
            seq,
            serde_json::from_str(&call).context("unreadable event")?,
        ))
    })
    .transpose()
}

fn remove_outbox(tx: &Transaction<'_>, seq: i64) -> Result<()> {
    tx.execute("DELETE FROM hub_outbox WHERE seq=?1", [seq])?;
    Ok(())
}

#[cfg(test)]
mod tests;
