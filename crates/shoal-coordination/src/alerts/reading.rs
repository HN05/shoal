//! Reading and clearing alerts.
use crate::{Coordination, daemon::Daemon};
use shoal_daemon_base::notifications::{chosen, merged};
use shoal_daemon_base::permitted_actions::Action;
use std::collections::{BTreeMap, HashSet};

use anyhow::{Result, ensure};
use rusqlite::{Connection, Transaction};
use shoal_ids::{member_ids::MemberId, space_ids::SpaceId};
use shoal_protocol::{
    alerts::Cleared, hub::alerts::AlertMarks, protocol::AlertsFor, space_names::SpaceSelector,
};

use super::Alert;
use crate::{config::permissions::PermissionKey, scope::Caller};

/// What reading the user's alerts takes from a scoped caller.
pub fn users_alerts() -> Action {
    Action::new(PermissionKey::Notifications, "the user's alerts")
}

impl Coordination {
    /// The alerts for `to`, oldest first: the oldest unread ones when
    /// `unread_only`, otherwise the newest `limit`. Every recipient's are
    /// those of the spaces `caller`'s scope reaches, with the user's when
    /// it may read them.
    pub async fn alerts_for(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        to: AlertsFor,
        unread_only: bool,
        limit: u32,
    ) -> Result<Vec<Alert>> {
        let every = match to {
            AlertsFor::User => return self.notifications.list(unread_only, limit).await,
            AlertsFor::Space { space } => {
                let id = daemon.space(&space).await?.id;
                let listed = move |db: &Transaction<'_>| listed(db, Some(&id), unread_only);
                return Ok(chosen(self.store.read(listed).await?, unread_only, limit));
            }
            AlertsFor::Every => self.store.read(move |db| listed(db, None, unread_only)),
        };
        let user = caller.is_none()
            || self
                .authorize_action(daemon, caller, users_alerts())
                .await
                .is_ok();
        let reached = match self.reach_root(caller).await? {
            Some(root) => Some(daemon.spaces_within(Some(&root)).await?),
            None => None,
        };
        let reached: Option<HashSet<SpaceId>> =
            reached.map(|spaces| spaces.into_iter().map(|space| space.id).collect());
        let readable = |alert: &Alert| match &alert.recipient {
            None => user,
            Some(recipient) => reached
                .as_ref()
                .is_none_or(|ids| ids.contains(&recipient.id)),
        };
        let here = every.await?.into_iter().filter(&readable).collect();
        let copies = match user {
            true => self.notifications.copies(),
            false => Vec::new(),
        };
        let elsewhere = copies
            .into_iter()
            .filter(|alert| !unread_only || !alert.read);
        Ok(chosen(merged(here, elsewhere), unread_only, limit))
    }

    /// Mark exactly the alerts read that `ids` number of this machine's:
    /// the user's, as `caller` may read them, and those for the agents of
    /// the spaces its scope reaches, whose agents then do not receive them
    /// again; and those to the user that `elsewhere` numbers for each other
    /// machine that keeps them, which the hub passes on to it.
    pub async fn mark_alerts_read(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        ids: Vec<i64>,
        elsewhere: BTreeMap<MemberId, Vec<i64>>,
    ) -> Result<()> {
        let recipients = (self.store).read(move |db| recipients(db, &ids)).await?;
        let mut users = Vec::new();
        let mut spaces: BTreeMap<SpaceId, Vec<i64>> = BTreeMap::new();
        for (id, recipient) in recipients {
            match recipient {
                None => users.push(id),
                Some(space) => spaces.entry(space).or_default().push(id),
            }
        }
        if let Some(caller) = caller {
            if !users.is_empty() || !elsewhere.is_empty() {
                self.authorize_action(daemon, Some(caller), users_alerts())
                    .await?;
            }
            for space in spaces.keys() {
                ensure!(
                    self.reaches(daemon, caller, space).await?,
                    "scoped processes read only the alerts of their own space and those below it"
                );
            }
        }
        if !users.is_empty() {
            self.notifications.mark_read(users).await?;
        }
        for (machine, read) in elsewhere {
            let marks = AlertMarks {
                read,
                ..AlertMarks::default()
            };
            self.mark_elsewhere(daemon, &machine, marks).await?;
        }
        for (space, ids) in spaces {
            (daemon.mark_messages_delivered(&SpaceSelector::id(&space), ids)).await?;
        }
        Ok(())
    }

    /// Delete every alert to the user this machine knows of, read or not:
    /// its own, and the other machines' as the hub last sent them, which
    /// those machines delete too, now or once they connect, leaving any
    /// they recorded since.
    pub async fn clear_users_alerts(&self, daemon: &dyn Daemon) -> Result<Cleared> {
        let mut cleared = self.notifications.clear().await?;
        let mut elsewhere: BTreeMap<MemberId, Vec<i64>> = BTreeMap::new();
        for alert in self.notifications.copies() {
            if let Some(machine) = alert.machine {
                elsewhere.entry(machine.id).or_default().push(alert.id);
            }
        }
        for (machine, deleted) in elsewhere {
            cleared.cleared += deleted.len() as u64;
            let marks = AlertMarks {
                deleted,
                ..AlertMarks::default()
            };
            self.mark_elsewhere(daemon, &machine, marks).await?;
        }
        Ok(cleared)
    }

    /// Mark the alerts `ids` read that the machine `machine` keeps, as a
    /// follow that delivered them names it.
    pub async fn mark_copies_read(
        &self,
        daemon: &dyn Daemon,
        machine: &MemberId,
        read: Vec<i64>,
    ) -> Result<()> {
        let marks = AlertMarks {
            read,
            ..AlertMarks::default()
        };
        self.mark_elsewhere(daemon, machine, marks).await
    }

    /// Take `marks` of `machine`'s alerts into this machine's copies at
    /// once, and send them to the hub.
    async fn mark_elsewhere(
        &self,
        daemon: &dyn Daemon,
        machine: &MemberId,
        marks: AlertMarks,
    ) -> Result<()> {
        self.notifications.mark_copies(machine, &marks);
        daemon.mark_alerts_elsewhere(machine, marks).await
    }
}

/// The alerts for the space `recipient`, or for every recipient without
/// one, only the unread ones when `unread_only`, oldest first.
fn listed(db: &Connection, recipient: Option<&SpaceId>, unread_only: bool) -> Result<Vec<Alert>> {
    let mut filters = Vec::new();
    if recipient.is_some() {
        filters.push("a.recipient_id=?1");
    }
    if unread_only {
        filters.push("a.read=0");
    }
    let filter = match filters.is_empty() {
        true => String::new(),
        false => format!("WHERE {}", filters.join(" AND ")),
    };
    let clauses = format!("{filter} ORDER BY a.id");
    match recipient {
        Some(recipient) => super::select(db, &clauses, [recipient]),
        None => super::select(db, &clauses, []),
    }
}

/// The space each of the alerts `ids` is for, none for the user; an alert
/// that is gone is left out.
fn recipients(db: &Connection, ids: &[i64]) -> Result<Vec<(i64, Option<SpaceId>)>> {
    let mut statement = db.prepare("SELECT recipient_id FROM alerts WHERE id=?1")?;
    let mut found = Vec::new();
    for &id in ids {
        let mut rows = statement.query([id])?;
        if let Some(row) = rows.next()? {
            found.push((id, row.get(0)?));
        }
    }
    Ok(found)
}

/// Mark read the user's unread alerts that the space `id` sent from its
/// chat, which a prompt the user submits there answers. Alerts are
/// one-way, so the prompt is no reply to them.
pub fn read_in_chat(tx: &Transaction<'_>, id: &SpaceId) -> Result<()> {
    tx.execute(
        "UPDATE alerts SET read=1 WHERE recipient_id IS NULL AND read=0 AND in_chat
            AND sender='space' AND space_id=?1",
        [id],
    )?;
    Ok(())
}

/// Record that the alerts `ids` carry a turn's final message.
pub fn mark_in_chat(tx: &Transaction<'_>, ids: &[i64]) -> Result<()> {
    for id in ids {
        tx.execute("UPDATE alerts SET in_chat=1 WHERE id=?1", [id])?;
    }
    Ok(())
}
