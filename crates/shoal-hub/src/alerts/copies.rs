//! As the hub: its copy of every machine's alerts to the user, which each
//! machine's reports keep current, and the changes to them, which every
//! member following them hears of, and the hub's own machine too.
use anyhow::{Result, anyhow};
use rusqlite::{Connection, Transaction, params};
use shoal_ids::member_ids::MemberId;
use shoal_protocol::{
    alerts::Alert,
    hub::{
        Machine,
        alerts::{AlertCopies, UsersAlerts},
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
    /// Tell every follower of `change`.
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
/// returning what changed of them, none when nothing did.
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
    let (alerts, mut removed) = match users {
        UsersAlerts::Whole(alerts) => {
            let reported: std::collections::HashSet<i64> = alerts.iter().map(|a| a.id).collect();
            let gone = (kept_numbers(tx, member)?.into_iter())
                .filter(|id| !reported.contains(id))
                .collect();
            (alerts, gone)
        }
        UsersAlerts::Changes { changed, removed } => (changed, removed),
    };
    for id in &removed {
        tx.execute(
            "DELETE FROM hub_user_alerts WHERE member_id=?1 AND id=?2",
            params![member, id],
        )?;
    }
    let mut changed = Vec::new();
    for mut alert in alerts {
        alert.machine = Some(machine.clone());
        let json = serde_json::to_string(&alert)?;
        let updated = tx.execute(
            "INSERT INTO hub_user_alerts(member_id, id, alert) VALUES (?1,?2,?3)
             ON CONFLICT(member_id, id) DO UPDATE SET alert=excluded.alert
                WHERE alert IS NOT excluded.alert",
            params![member, alert.id, json],
        )?;
        if updated > 0 {
            changed.push(alert);
        }
    }
    removed.sort_unstable();
    Ok(
        (!changed.is_empty() || !removed.is_empty()).then(|| AlertCopies::Changed {
            machine: member.clone(),
            changed,
            removed,
        }),
    )
}

/// Every machine's alerts to the user but `except`'s.
pub(crate) fn others(db: &Connection, except: &MemberId) -> Result<Vec<Alert>> {
    let mut statement =
        db.prepare("SELECT alert FROM hub_user_alerts WHERE member_id<>?1 ORDER BY member_id, id")?;
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

fn kept_numbers(db: &Connection, member: &MemberId) -> Result<Vec<i64>> {
    let mut statement = db.prepare("SELECT id FROM hub_user_alerts WHERE member_id=?1")?;
    let rows = statement.query_map([member], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests;
