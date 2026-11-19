//! What the hub holds for a joined machine while it is offline: requests
//! that can wait for it, such as messages, answers, decisions and stops,
//! and the events spaces pass up to its spaces. Once the machine connects
//! the hub delivers them in the order they came; one still waiting after
//! [`LIFETIME`] is dropped, and the machine that sent it hears of it.
use std::{sync::Arc, time::Duration};

use anyhow::{Context as _, Result};
use rusqlite::{OptionalExtension as _, Transaction, params};
use shoal_ids::{member_ids::MemberId, space_ids::SpaceId};
use shoal_protocol::hub::{
    Member,
    link::{Call, Reply},
    routing::{Expired, HeldOutcome, Queued},
};
use shoal_protocol::protocol::{Body, Response};

use super::session::{Calls, Unanswered, Unsent};
use crate::{
    daemon::{log, manager::Manager},
    time::UnixTime,
};

/// How long the hub holds a request or event for an offline machine.
pub(super) const LIFETIME: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// How often the hub drops what expired.
const SWEEP: Duration = Duration::from_secs(10 * 60);

/// A call the hub holds for an offline machine, with who sent it and what
/// it is, which the sender hears of should it expire.
#[derive(Debug, Clone)]
pub(super) struct Held {
    pub sender: MemberId,
    pub sender_space: Option<SpaceId>,
    pub call: Call,
    pub summary: String,
}

/// A held call that expired, or that the hub delivered, with the machine
/// it was for.
struct Lapsed {
    machine: MemberId,
    held: Held,
    queued_at: UnixTime,
}

impl Manager {
    /// Hold `held` for the offline machine `machine` until it connects.
    pub(super) async fn hold_for(&self, machine: &Member, held: Held) -> Result<Queued> {
        let now = UnixTime::now();
        let expires_at = now + LIFETIME;
        let to = machine.id.clone();
        (self.store)
            .write(move |tx| push(tx, &to, &held, now, expires_at))
            .await?;
        // Its link may have opened since the hub looked.
        self.hub.deliveries.of(&machine.id).wake.notify_one();
        Ok(Queued {
            machine: machine.id.clone(),
            name: machine.name.clone(),
            last_seen: machine.last_seen,
            expires_at,
        })
    }

    /// Deliver what the hub holds for `member` over its link `link`, as it
    /// comes, until another link replaces it or it closes.
    pub(super) async fn keep_delivering(self: Arc<Self>, member: MemberId, link: Arc<Calls>) {
        let delivery = self.hub.deliveries.of(&member);
        loop {
            self.deliver_held(&member, &delivery).await;
            // What the hub holds meanwhile wakes it; a lost wake, at worst,
            // waits for the next look.
            tokio::select! {
                () = delivery.wake.notified() => {}
                () = tokio::time::sleep(SWEEP) => {}
            }
            let open = self.hub.links.calls(&member);
            if !open.is_some_and(|calls| Arc::ptr_eq(&calls, &link)) {
                return;
            }
        }
    }

    /// Deliver what the hub holds for `member`, oldest first, until none is
    /// left or the link closes. A call that never left stays for the next
    /// link; one the machine received but never answered is not sent
    /// again, since it may have run, and its sender hears that it went
    /// unconfirmed, as of one the machine refused or that expired first.
    async fn deliver_held(&self, member: &MemberId, delivery: &Delivery) {
        let _delivering = delivery.lock.lock().await;
        if let Err(error) = self.drop_expired().await {
            return log!("hub: could not drop expired calls: {error:#}");
        }
        // Sent before the hub stopped, without its machine's answer.
        let to = member.clone();
        match self.store.write(move |tx| take_claimed(tx, &to)).await {
            Ok(uncertain) => {
                for lapsed in uncertain {
                    self.tell_sender(lapsed, HeldOutcome::Unconfirmed).await;
                }
            }
            Err(error) => return log!("hub: could not read what it held: {error:#}"),
        }
        loop {
            let (to, now) = (member.clone(), UnixTime::now());
            let next = match self.store.write(move |tx| claim_first(tx, &to, now)).await {
                Ok(Some(next)) => next,
                Ok(None) => return,
                Err(error) => return log!("hub: could not read what it holds: {error:#}"),
            };
            let (seq, lapsed) = next;
            let Some(calls) = self.hub.links.calls(member) else {
                return self.release_claim(seq).await;
            };
            let outcome = match calls.call(lapsed.held.call.clone()).await {
                Ok(reply) => refusal(&reply).map(|reason| HeldOutcome::Refused { reason }),
                Err(error) if error.is::<Unsent>() => return self.release_claim(seq).await,
                Err(error) if error.is::<Unanswered>() => Some(HeldOutcome::Unconfirmed),
                Err(error) => Some(HeldOutcome::Refused {
                    reason: format!("{error:#}"),
                }),
            };
            if let Some(outcome) = outcome {
                self.tell_sender(lapsed, outcome).await;
            }
            if let Err(error) = self.store.write(move |tx| remove(tx, seq)).await {
                return log!("hub: could not drop a delivered call: {error:#}");
            }
        }
    }

    /// Let the held call `seq`, which never left, go with the next link.
    async fn release_claim(&self, seq: i64) {
        let released = self.store.write(move |tx| release(tx, seq)).await;
        if let Err(error) = released {
            log!("hub: could not keep a held call for the next link: {error:#}");
        }
    }

    /// Drop what the hub held past its lifetime, every [`SWEEP`] while this
    /// daemon is the hub, telling each sender what expired.
    pub(super) async fn expire_held(&self) {
        let mut sweep = tokio::time::interval(SWEEP);
        loop {
            sweep.tick().await;
            if !self.is_hub() {
                continue;
            }
            if let Err(error) = self.drop_expired().await {
                log!("hub: could not drop expired calls: {error:#}");
            }
        }
    }

    async fn drop_expired(&self) -> Result<()> {
        let now = UnixTime::now();
        let lapsed = self.store.write(move |tx| take_expired(tx, now)).await?;
        if lapsed.is_empty() {
            return Ok(());
        }
        for lapsed in lapsed {
            self.tell_sender(lapsed, HeldOutcome::Expired).await;
        }
        Ok(())
    }
}

/// Why the machine refused what `reply` answers, a forwarded request whose
/// response is an error.
fn refusal(reply: &Reply) -> Option<String> {
    let Reply::Answered(answered) = reply else {
        return None;
    };
    let mut responses = (answered.frames.iter())
        .filter_map(|frame| serde_json::from_value::<Response>(frame.clone()).ok());
    responses.find_map(|response| match response.body {
        Body::Error { message, .. } => Some(message),
        _ => None,
    })
}

impl Manager {
    /// Tell the sender of `lapsed` its `outcome`; what tells a sender of
    /// one is not reported in turn.
    async fn tell_sender(&self, lapsed: Lapsed, outcome: HeldOutcome) {
        let Lapsed {
            machine,
            held,
            queued_at,
        } = lapsed;
        if matches!(held.call, Call::Expired { .. }) {
            return;
        }
        let members = self
            .local_hub_overview()
            .await
            .map(|overview| overview.members);
        let name = (members.iter().flatten())
            .find(|member| member.id == machine)
            .map_or_else(|| machine.to_string(), |member| member.name.clone());
        let expired = Expired {
            from: held.sender_space,
            machine: name,
            summary: held.summary,
            queued_at,
            outcome,
        };
        if let Err(error) = self.send_expired(&held.sender, expired).await {
            log!("hub: could not tell a sender what became of its call: {error:#}");
        }
    }
}

fn push(
    tx: &Transaction<'_>,
    to: &MemberId,
    held: &Held,
    now: UnixTime,
    expires_at: UnixTime,
) -> Result<()> {
    tx.execute(
        "INSERT INTO hub_queue
            (member_id, sender_id, sender_space, call, summary, queued_at, expires_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            to,
            held.sender,
            held.sender_space,
            serde_json::to_string(&held.call)?,
            held.summary,
            now,
            expires_at,
        ],
    )?;
    Ok(())
}

/// Claim the oldest call held for `member` that the hub has not sent and
/// that has not expired by `now`, with its place in the queue.
fn claim_first(
    tx: &Transaction<'_>,
    member: &MemberId,
    now: UnixTime,
) -> Result<Option<(i64, Lapsed)>> {
    let row = tx
        .query_row(
            &format!("SELECT {COLUMNS} FROM hub_queue WHERE member_id=?1 AND NOT delivering AND expires_at > ?2 ORDER BY seq LIMIT 1"),
            params![member, now],
            row,
        )
        .optional()?;
    let Some((seq, lapsed)) = row.map(decode).transpose()? else {
        return Ok(None);
    };
    tx.execute("UPDATE hub_queue SET delivering=1 WHERE seq=?1", [seq])?;
    Ok(Some((seq, lapsed)))
}

/// Take the calls held for `member` that the hub sent without hearing
/// back, oldest first, as when it stopped meanwhile.
fn take_claimed(tx: &Transaction<'_>, member: &MemberId) -> Result<Vec<Lapsed>> {
    let rows = tx
        .prepare(&format!(
            "SELECT {COLUMNS} FROM hub_queue WHERE member_id=?1 AND delivering ORDER BY seq"
        ))?
        .query_map([member], row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    tx.execute(
        "DELETE FROM hub_queue WHERE member_id=?1 AND delivering",
        [member],
    )?;
    rows.into_iter().map(|row| Ok(decode(row)?.1)).collect()
}

fn release(tx: &Transaction<'_>, seq: i64) -> Result<()> {
    tx.execute("UPDATE hub_queue SET delivering=0 WHERE seq=?1", [seq])?;
    Ok(())
}

/// The columns [`row`] reads.
const COLUMNS: &str = "seq, member_id, sender_id, sender_space, call, summary, queued_at";

type Row = (
    i64,
    MemberId,
    MemberId,
    Option<SpaceId>,
    String,
    String,
    UnixTime,
);

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
    ))
}

/// The held call `row` holds, with its place in the queue.
fn decode(row: Row) -> Result<(i64, Lapsed)> {
    let (seq, machine, sender, sender_space, call, summary, queued_at) = row;
    let held = Held {
        sender,
        sender_space,
        call: serde_json::from_str(&call).context("unreadable held call")?,
        summary,
    };
    let lapsed = Lapsed {
        machine,
        held,
        queued_at,
    };
    Ok((seq, lapsed))
}

fn remove(tx: &Transaction<'_>, seq: i64) -> Result<()> {
    tx.execute("DELETE FROM hub_queue WHERE seq=?1", [seq])?;
    Ok(())
}

/// Take the calls held past `now`, oldest first.
fn take_expired(tx: &Transaction<'_>, now: UnixTime) -> Result<Vec<Lapsed>> {
    let rows = tx
        .prepare(&format!(
            "SELECT {COLUMNS} FROM hub_queue WHERE expires_at <= ?1 AND NOT delivering ORDER BY seq"
        ))?
        .query_map([now], row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    tx.execute(
        "DELETE FROM hub_queue WHERE expires_at <= ?1 AND NOT delivering",
        [now],
    )?;
    rows.into_iter().map(|row| Ok(decode(row)?.1)).collect()
}

/// Drop what the hub holds for `member`, which it revoked.
pub(super) fn forget(tx: &Transaction<'_>, member: &MemberId) -> Result<()> {
    tx.execute("DELETE FROM hub_queue WHERE member_id=?1", [member])?;
    Ok(())
}

/// One delivery per machine: a lock a delivery of what the hub holds for
/// it takes, so a claim it finds was left by a hub that stopped, and what
/// wakes it when the hub holds something new.
#[derive(Default)]
pub(super) struct Deliveries(std::sync::Mutex<std::collections::HashMap<MemberId, Arc<Delivery>>>);

#[derive(Default)]
pub(super) struct Delivery {
    lock: tokio::sync::Mutex<()>,
    wake: tokio::sync::Notify,
}

impl Deliveries {
    fn of(&self, member: &MemberId) -> Arc<Delivery> {
        let mut deliveries = self.0.lock().unwrap();
        deliveries.entry(member.clone()).or_default().clone()
    }
}

#[cfg(test)]
mod tests;
