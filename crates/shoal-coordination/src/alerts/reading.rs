//! Reading alerts: listing those for the user, a space or every recipient a
//! caller may read, and marking those a caller read.
use crate::{Coordination, daemon::Daemon};
use shoal_daemon_base::notifications::{chosen, merged};
use shoal_daemon_base::permitted_actions::Action;
use std::collections::{BTreeMap, HashSet};

use anyhow::{Result, ensure};
use rusqlite::{Connection, Transaction};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{protocol::AlertsFor, space_names::SpaceSelector};

use super::{Alert, COLUMNS, FROM};
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

    /// Mark exactly the alerts `ids` read: the user's, as `caller` may read
    /// them, and those for the agents of the spaces its scope reaches,
    /// whose agents then do not receive them again.
    pub async fn mark_alerts_read(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        ids: Vec<i64>,
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
            if !users.is_empty() {
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
        for (space, ids) in spaces {
            (daemon.mark_messages_delivered(&SpaceSelector::id(&space), ids)).await?;
        }
        Ok(())
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
    let mut statement = db.prepare(&format!(
        "SELECT {COLUMNS} FROM {FROM} {filter} ORDER BY a.id"
    ))?;
    let rows = match recipient {
        Some(recipient) => statement.query_map([recipient], super::row)?,
        None => statement.query_map([], super::row)?,
    };
    Ok(rows.collect::<rusqlite::Result<_>>()?)
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
