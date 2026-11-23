//! Events that go up a tree spanning machines: what a space passes to its
//! coordinator goes to the machine of the first space above it that runs
//! elsewhere, which hands it to the nearest space from there up with an
//! agent, passes it on to the next machine where the tree goes on, or
//! tells the user at the top. While a machine's link is down, what its
//! spaces pass up waits in its outbox and goes in order once the link
//! opens again; the hub holds what it cannot deliver yet.
use anyhow::{Context as _, Result};
use rusqlite::{OptionalExtension as _, Transaction, params};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    alerts::AlertKind,
    files::SentFile,
    hub::{
        link::Call,
        routing::{Expired, HeldOutcome, Told, ToldAbout, ToldRecord},
    },
    space_names::SpaceName,
};

use super::{membership, session::Calls};
use crate::{Hub, daemon::Daemon, daemon::Receiver, log, time::UnixTime};
use shoal_daemon_base::alerts::{About, NewAlert, Sender};

/// How often the outbox looks for what it keeps, besides when it keeps
/// something new.
const OUTBOX_LOOK: std::time::Duration = std::time::Duration::from_secs(60);

/// An event a space passes up: the space it is about, the kind of alert it
/// is, whether the user receives it when no space above takes it, its
/// message and the records of the files it sent.
pub struct Event<'a> {
    pub name: &'a SpaceName,
    pub space: &'a SpaceId,
    pub kind: AlertKind,
    pub for_user: bool,
    pub message: String,
    /// What the agent that takes it reads, where that differs from
    /// `message`, which the user reads, as a thread's post does.
    pub agent: Option<String>,
    pub files: &'a [SentFile],
}

impl Hub {
    /// Pass `event` up to the space `above`, which another machine runs.
    pub async fn pass_up(
        &self,
        daemon: &dyn Daemon,
        above: SpaceId,
        event: Event<'_>,
    ) -> Result<()> {
        let told = told_above(above, event, &self.machine_name(daemon).await);
        self.send_told(daemon, told).await
    }

    /// Pass `event` up to the space `above`, which another machine runs,
    /// only once the hub takes it, which holds it while that machine is
    /// offline: an error leaves it with the caller, as while this
    /// machine's link is down or the hub or that machine refused it.
    pub async fn pass_up_now(
        &self,
        daemon: &dyn Daemon,
        above: SpaceId,
        event: Event<'_>,
    ) -> Result<()> {
        let told = told_above(above, event, &self.machine_name(daemon).await);
        if self.is_hub(daemon) {
            return (self
                .route_told(daemon, &self.own_member().await?, told)
                .await)
                .map(drop);
        }
        let calls = (self.member.calls()).context("this machine is not connected to the hub")?;
        calls.call(Call::Tell { told }).await.map(drop)
    }

    /// Whether this machine reaches the hub now: it is the hub, or its
    /// link to the hub is open.
    pub fn reaches_hub(&self, daemon: &dyn Daemon) -> bool {
        self.is_hub(daemon) || self.member.calls().is_some()
    }

    /// Message the agent of the space `id`, which another machine runs.
    pub async fn message_elsewhere(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        kind: AlertKind,
        message: String,
    ) -> Result<()> {
        let told = Told {
            above: id.clone(),
            about: None,
            for_user: false,
            record: ToldRecord::Alert {
                kind,
                message,
                files: Vec::new(),
            },
        };
        self.send_told(daemon, told).await
    }

    /// Send `told` through the hub: this daemon's own routing on the hub,
    /// else this machine's link, or its outbox while the link is down.
    pub(super) async fn send_told(&self, daemon: &dyn Daemon, told: Told) -> Result<()> {
        if self.is_hub(daemon) {
            return (self
                .route_told(daemon, &self.own_member().await?, told)
                .await)
                .map(drop);
        }
        if self.store.read(membership::load).await?.is_none() {
            anyhow::bail!("this machine joined no hub to pass the event to");
        }
        self.call_or_hold(Call::Tell { told }).await
    }

    /// Make `call` over this machine's link, or keep it in the outbox,
    /// which sends it once the link opens, while the link is down or the
    /// call fails.
    pub(crate) async fn call_or_hold(&self, call: Call) -> Result<()> {
        if let Some(calls) = self.member.calls()
            && calls.call(call.clone()).await.is_ok()
        {
            return Ok(());
        }
        let queued_at = UnixTime::now();
        (self.store)
            .write(move |tx| push_outbox(tx, &call, queued_at))
            .await?;
        // The link may have opened since this looked.
        self.outbox.notify_one();
        Ok(())
    }

    /// Send what the outbox keeps over the link `calls` serves, as it comes,
    /// for as long as the link lasts.
    pub(super) async fn keep_sending_outbox(&self, calls: &Calls) -> std::convert::Infallible {
        loop {
            self.send_outbox(calls).await;
            // A lost wake, at worst, waits for the next look.
            tokio::select! {
                () = self.outbox.notified() => {}
                () = tokio::time::sleep(OUTBOX_LOOK) => {}
            }
        }
    }

    /// Send what waited in the outbox for the link `calls` serves, oldest
    /// first, until none is left or a call fails, which keeps it for the
    /// next link.
    async fn send_outbox(&self, calls: &Calls) {
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
                Err(error) if self.member.calls().is_some() => {
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
    pub(super) async fn deliver_told(&self, daemon: &dyn Daemon, told: Told) -> Result<()> {
        let receiver = daemon.receiver_from(&told.above).await?;
        let kind = told.record.kind();
        let (agent, user) = told.record.texts();
        match (receiver, told.about) {
            (Some(Receiver::Here(coordinator)), about) => {
                let about = about.map_or_else(About::default, |about| {
                    About::space(&about.name, &about.space)
                });
                let files = self.files_here(daemon, told.record.files()).await?;
                let alert = told_alert(kind, about, agent, files);
                (daemon.alert(&coordinator, alert, told.for_user)).await
            }
            (Some(Receiver::Elsewhere(next)), about) => {
                let told = Told {
                    above: next,
                    about,
                    ..told
                };
                self.send_told(daemon, told).await
            }
            (None, Some(about)) => {
                if told.for_user {
                    let about = About::space(&about.name, &about.space);
                    let files = self.files_here(daemon, told.record.files()).await?;
                    let alert = told_alert(kind, about, user, files);
                    self.notifications.notify_user(alert).await;
                }
                Ok(())
            }
            (None, None) => anyhow::bail!("space {} has no agent to tell", told.above),
        }
    }

    /// `files` as this machine reads them: those it keeps itself by their
    /// copy's path, as when an event it passed up comes back to it, without
    /// those whose record is gone, as an alert of its own space shows them.
    async fn files_here(&self, daemon: &dyn Daemon, files: &[SentFile]) -> Result<Vec<SentFile>> {
        let machine = self.machine_name(daemon).await;
        let (back, elsewhere): (Vec<_>, Vec<_>) =
            (files.iter().cloned()).partition(|file| file.copy.is_on(&machine));
        if back.is_empty() {
            return Ok(elsewhere);
        }
        let mut here = daemon.kept_copies(back).await?;
        here.extend(elsewhere);
        Ok(here)
    }

    /// Tell whoever sent what expired at the hub: the space that sent it,
    /// when it still has an agent here, else the user.
    pub(super) async fn hear_expired(&self, daemon: &dyn Daemon, expired: Expired) {
        let (summary, machine) = (&expired.summary, &expired.machine);
        let queued = expired.queued_at.local_minutes();
        let message = match &expired.outcome {
            HeldOutcome::Expired => format!(
                "The hub dropped {summary}, which waited for machine {machine} from {queued} and expired before that machine connected"
            ),
            HeldOutcome::Unconfirmed => format!(
                "The hub delivered {summary}, which waited for machine {machine} from {queued}, but the link closed before {machine} confirmed it ran; check there before sending it again"
            ),
            HeldOutcome::Refused { reason } => format!(
                "Machine {machine} refused {summary}, which waited for it from {queued}: {reason}"
            ),
        };
        if let Some(space) = &expired.from
            && (daemon
                .message(space, AlertKind::HubExpired, message.clone())
                .await)
                .is_ok()
        {
            return;
        }
        self.notify(
            daemon,
            shoal_daemon_base::notifications::About::named(None),
            AlertKind::HubExpired,
            message.to_string(),
        )
        .await;
    }
}

/// The alert of `kind` with `message` about the space `about`, with its
/// `files`, that a told event becomes: that space's own message, which it
/// may withdraw, or else Shoal's alert about it.
fn told_alert(kind: AlertKind, about: About, message: String, files: Vec<SentFile>) -> NewAlert {
    let sender = match kind {
        AlertKind::Message if about.id.is_some() => Sender::Space,
        _ => Sender::Shoal,
    };
    let alert = NewAlert {
        sender,
        ..NewAlert::shoal(kind, about, message)
    };
    alert.with_files(&files)
}

/// What tells the space `above` of `event`, whose files `machine`, this
/// one, keeps.
fn told_above(above: SpaceId, event: Event<'_>, machine: &str) -> Told {
    let files = (event.files.iter())
        .map(|file| {
            let mut file = file.clone();
            file.copy.kept_on(machine);
            file
        })
        .collect();
    Told {
        above,
        about: Some(ToldAbout {
            space: event.space.clone(),
            name: event.name.clone(),
        }),
        for_user: event.for_user,
        record: match event.agent {
            Some(agent) => ToldRecord::Thread {
                kind: event.kind,
                agent,
                user: event.message,
                files,
            },
            None => ToldRecord::Alert {
                kind: event.kind,
                message: event.message,
                files,
            },
        },
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
