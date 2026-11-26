//! The hub's copy of every machine's alerts to the user. What another
//! machine did to an alert stays on the copy until the alert's own machine
//! reports doing the same, so a report sent meanwhile undoes nothing.
use anyhow::{Result, anyhow};
use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension as _, Transaction, params};
use shoal_ids::member_ids::MemberId;
use shoal_protocol::{
    alerts::Alert,
    hub::{
        Machine,
        alerts::{AlertCopies, AlertMarks, UsersAlerts},
    },
};
use tokio::sync::broadcast;

use crate::members;

/// How many changes a follower may fall behind before it reads every copy
/// again.
const BACKLOG: usize = 256;

/// The changes to the hub's copies, as they come.
pub(crate) struct Changes(broadcast::Sender<AlertCopies>);

impl Default for Changes {
    fn default() -> Self {
        Self(broadcast::channel(BACKLOG).0)
    }
}

impl Changes {
    pub(crate) fn send(&self, change: AlertCopies) {
        // None follows while no member does.
        let _ = self.0.send(change);
    }

    /// Hear of the changes from now on.
    pub(crate) fn follow(&self) -> broadcast::Receiver<AlertCopies> {
        self.0.subscribe()
    }
}

/// Keep `member`'s report of its alerts to the user as the hub's copies,
/// returning what changed of them, none when nothing did. A copy another
/// machine read stays read, and one it cleared stays out, until `member`
/// reports doing the same.
pub(crate) fn record(
    tx: &Transaction<'_>,
    member: &MemberId,
    users: UsersAlerts,
) -> Result<Option<AlertCopies>> {
    let name =
        members::name(tx, member)?.ok_or_else(|| anyhow!("the hub has no member {member}"))?;
    let machine = Machine {
        id: member.clone(),
        name,
    };
    let (alerts, gone) = match users {
        UsersAlerts::Whole(alerts) => {
            let reported: HashSet<i64> = alerts.iter().map(|alert| alert.id).collect();
            let gone = (kept_numbers(tx, member)?.into_iter())
                .filter(|id| !reported.contains(id))
                .collect();
            (alerts, gone)
        }
        UsersAlerts::Changes { changed, removed } => (changed, removed),
    };
    let mut removed = Vec::new();
    for id in gone {
        let shown = !marks_of(tx, member, id)?.is_some_and(|marks| marks.deleted);
        let deleted = tx.execute(
            "DELETE FROM hub_user_alerts WHERE member_id=?1 AND id=?2",
            params![member, id],
        )?;
        if deleted > 0 && shown {
            removed.push(id);
        }
    }
    let mut changed = Vec::new();
    for mut alert in alerts {
        alert.machine = Some(machine.clone());
        let marks = marks_of(tx, member, alert.id)?.unwrap_or_default();
        if marks.deleted {
            continue;
        }
        let read_marked = marks.read && !alert.read;
        alert.read |= marks.read;
        let updated = tx.execute(
            "INSERT INTO hub_user_alerts(member_id, id, alert, read_marked) VALUES (?1,?2,?3,?4)
             ON CONFLICT(member_id, id) DO UPDATE
                SET alert=excluded.alert, read_marked=excluded.read_marked
                WHERE alert IS NOT excluded.alert OR read_marked IS NOT excluded.read_marked",
            params![
                member,
                alert.id,
                serde_json::to_string(&alert)?,
                read_marked
            ],
        )?;
        if updated > 0 {
            changed.push(alert);
        }
    }
    Ok(changes(member, changed, removed))
}

/// Take what another machine did to `member`'s alerts to the user into
/// the hub's copies, returning what changed of them, none when nothing did.
pub(crate) fn mark(
    tx: &Transaction<'_>,
    member: &MemberId,
    marks: &AlertMarks,
) -> Result<Option<AlertCopies>> {
    let mut changed = Vec::new();
    for &id in &marks.read {
        let Some(mut alert) = shown(tx, member, id)? else {
            continue;
        };
        if alert.read {
            continue;
        }
        alert.read = true;
        tx.execute(
            "UPDATE hub_user_alerts SET alert=?3, read_marked=1 WHERE member_id=?1 AND id=?2",
            params![member, id, serde_json::to_string(&alert)?],
        )?;
        changed.push(alert);
    }
    let mut removed = Vec::new();
    for &id in &marks.deleted {
        let deleted = tx.execute(
            "UPDATE hub_user_alerts SET deleted=1 WHERE member_id=?1 AND id=?2 AND deleted=0",
            params![member, id],
        )?;
        if deleted > 0 {
            removed.push(id);
        }
    }
    Ok(changes(member, changed, removed))
}

/// What other machines did to `member`'s alerts to the user that it has
/// not reported doing yet.
pub(crate) fn pending(db: &Connection, member: &MemberId) -> Result<AlertMarks> {
    let mut statement = db.prepare(
        "SELECT id, deleted FROM hub_user_alerts
            WHERE member_id=?1 AND (read_marked=1 OR deleted=1) ORDER BY id",
    )?;
    let rows = statement.query_map([member], |row| Ok((row.get(0)?, row.get(1)?)));
    let mut marks = AlertMarks::default();
    for row in rows? {
        let (id, deleted): (i64, bool) = row?;
        match deleted {
            true => marks.deleted.push(id),
            false => marks.read.push(id),
        }
    }
    Ok(marks)
}

/// Every machine's alerts to the user but `except`'s, without those
/// another machine cleared.
pub(crate) fn others(db: &Connection, except: &MemberId) -> Result<Vec<Alert>> {
    let mut statement = db.prepare(
        "SELECT alert FROM hub_user_alerts WHERE member_id<>?1 AND deleted=0
            ORDER BY member_id, id",
    )?;
    let rows = statement.query_map([except], |row| row.get::<_, String>(0))?;
    rows.map(|alert| Ok(serde_json::from_str(&alert?)?))
        .collect()
}

/// Drop `member`'s alerts, as once the hub revoked it.
pub(crate) fn forget(tx: &Transaction<'_>, member: &MemberId) -> Result<()> {
    tx.execute("DELETE FROM hub_user_alerts WHERE member_id=?1", [member])?;
    Ok(())
}

/// Whether `change` is about `member`'s own alerts, which it keeps itself.
pub(crate) fn about(change: &AlertCopies, member: &MemberId) -> bool {
    match change {
        AlertCopies::Changed { machine, .. } | AlertCopies::Dropped { machine } => {
            machine == member
        }
        AlertCopies::Whole(_) => false,
    }
}

/// The change to `member`'s copies that `changed` and `removed` make, none
/// when they are empty.
fn changes(member: &MemberId, changed: Vec<Alert>, mut removed: Vec<i64>) -> Option<AlertCopies> {
    removed.sort_unstable();
    (!changed.is_empty() || !removed.is_empty()).then(|| AlertCopies::Changed {
        machine: member.clone(),
        changed,
        removed,
    })
}

/// What another machine did to `member`'s alert `id` that it has not
/// reported doing, none without a copy of it.
fn marks_of(db: &Connection, member: &MemberId, id: i64) -> Result<Option<Marks>> {
    Ok(db
        .query_row(
            "SELECT read_marked, deleted FROM hub_user_alerts WHERE member_id=?1 AND id=?2",
            params![member, id],
            |row| {
                Ok(Marks {
                    read: row.get(0)?,
                    deleted: row.get(1)?,
                })
            },
        )
        .optional()?)
}

/// The hub's copy of `member`'s alert `id`, unless another machine
/// cleared it.
fn shown(db: &Connection, member: &MemberId, id: i64) -> Result<Option<Alert>> {
    let alert: Option<String> = db
        .query_row(
            "SELECT alert FROM hub_user_alerts WHERE member_id=?1 AND id=?2 AND deleted=0",
            params![member, id],
            |row| row.get(0),
        )
        .optional()?;
    alert
        .map(|alert| Ok(serde_json::from_str(&alert)?))
        .transpose()
}

/// What other machines did to an alert that its machine has not reported
/// doing.
#[derive(Default)]
struct Marks {
    read: bool,
    deleted: bool,
}

fn kept_numbers(db: &Connection, member: &MemberId) -> Result<Vec<i64>> {
    let mut statement = db.prepare("SELECT id FROM hub_user_alerts WHERE member_id=?1")?;
    let rows = statement.query_map([member], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests;
