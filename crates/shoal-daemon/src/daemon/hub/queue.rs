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
    link::Call,
    routing::{Expired, Queued},
};

use crate::{
    daemon::{log, workspace::Manager},
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

/// A held call that expired, with the machine it was for.
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
        Ok(Queued {
            machine: machine.id.clone(),
            name: machine.name.clone(),
            last_seen: machine.last_seen,
            expires_at,
        })
    }

    /// Deliver what the hub holds for `member`, oldest first, now that its
    /// link is open, until none is left or the link closes.
    pub(super) async fn deliver_held(self: Arc<Self>, member: MemberId) {
        loop {
            let to = member.clone();
            let next = match self.store.read(move |tx| first(tx, &to)).await {
                Ok(Some(next)) => next,
                Ok(None) => return,
                Err(error) => return log!("hub: could not read what it holds: {error:#}"),
            };
            let Some(calls) = self.hub.links.calls(&member) else {
                return;
            };
            let (seq, call) = next;
            if let Err(error) = calls.call(call).await {
                // A closed link keeps the call for the next one.
                if self.hub.links.calls(&member).is_none() {
                    return;
                }
                log!("hub: a held call failed at its machine: {error:#}");
            }
            if let Err(error) = self.store.write(move |tx| remove(tx, seq)).await {
                return log!("hub: could not drop a delivered call: {error:#}");
            }
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
        let machines = self.local_hub_overview().await?.members;
        for Lapsed {
            machine,
            held,
            queued_at,
        } in lapsed
        {
            // What tells a sender of an expiry is not reported in turn.
            if matches!(held.call, Call::Expired { .. }) {
                continue;
            }
            let name = (machines.iter().find(|member| member.id == machine))
                .map_or_else(|| machine.to_string(), |member| member.name.clone());
            let expired = Expired {
                from: held.sender_space,
                machine: name,
                summary: held.summary,
                queued_at,
            };
            if let Err(error) = self.send_expired(&held.sender, expired).await {
                log!("hub: could not tell a sender what expired: {error:#}");
            }
        }
        Ok(())
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

/// The oldest call held for `member`, with its place in the queue.
fn first(tx: &Transaction<'_>, member: &MemberId) -> Result<Option<(i64, Call)>> {
    let row = tx
        .query_row(
            "SELECT seq, call FROM hub_queue WHERE member_id=?1 ORDER BY seq LIMIT 1",
            [member],
            |row| Ok((row.get(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    row.map(|(seq, call)| {
        Ok((
            seq,
            serde_json::from_str(&call).context("unreadable held call")?,
        ))
    })
    .transpose()
}

fn remove(tx: &Transaction<'_>, seq: i64) -> Result<()> {
    tx.execute("DELETE FROM hub_queue WHERE seq=?1", [seq])?;
    Ok(())
}

/// Take the calls held past `now`, oldest first.
fn take_expired(tx: &Transaction<'_>, now: UnixTime) -> Result<Vec<Lapsed>> {
    let rows = tx
        .prepare(
            "SELECT member_id, sender_id, sender_space, call, summary, queued_at FROM hub_queue
                WHERE expires_at <= ?1 ORDER BY seq",
        )?
        .query_map([now], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get::<_, String>(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    tx.execute("DELETE FROM hub_queue WHERE expires_at <= ?1", [now])?;
    rows.into_iter()
        .map(
            |(machine, sender, sender_space, call, summary, queued_at)| {
                Ok(Lapsed {
                    machine,
                    held: Held {
                        sender,
                        sender_space,
                        call: serde_json::from_str(&call).context("unreadable held call")?,
                        summary,
                    },
                    queued_at,
                })
            },
        )
        .collect()
}

/// Drop what the hub holds for `member`, which it revoked.
pub(super) fn forget(tx: &Transaction<'_>, member: &MemberId) -> Result<()> {
    tx.execute("DELETE FROM hub_queue WHERE member_id=?1", [member])?;
    Ok(())
}

#[cfg(test)]
mod tests;
