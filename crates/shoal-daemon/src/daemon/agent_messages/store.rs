//! The alerts for a space's agent: queueing one, listing the unread ones as
//! the agent reads them, marking delivered ones read, and what becomes of
//! them when their space goes.
use anyhow::{Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::{Delivery, MAX_UNREAD, MAX_UNREAD_BYTES, Queued, Unread};
use crate::{
    daemon::{
        alerts::{self, About, Alert, COLUMNS, FROM, NewAlert},
        notifications,
    },
    state::WorkspaceState,
};
use shoal_ids::space_ids::SpaceId;

/// Queue `alert` for the agent of the space `space_id` in the caller's
/// transaction, `for_user` when the user receives it should the space go
/// before its agent reads it: its ID, or that of an identical alert from
/// the same sender about the same space that is still unread, which it
/// does not repeat. An identical unread alert that does not wake the agent
/// wakes it once queued again with [`Delivery::Wake`]. A workspace being removed takes none, since
/// removal decides from the alerts it has.
pub(super) fn insert(
    tx: &Transaction<'_>,
    space_id: &SpaceId,
    alert: &NewAlert,
    delivery: Delivery,
    for_user: bool,
) -> Result<Queued> {
    let attachments = alert.attachments_column()?;
    let present: Option<bool> = tx
        .query_row(
            "SELECT state IS NOT ?2 FROM spaces LEFT JOIN workspaces USING(id) WHERE id=?1",
            params![space_id, WorkspaceState::Removing],
            |row| row.get(0),
        )
        .optional()?;
    ensure!(present.is_some(), "no space {space_id}");
    ensure!(present == Some(true), "the workspace is being removed");
    let wakes = delivery == Delivery::Wake;
    // Names repeat: an alert names the space its ID names, and is the same
    // as another only when both name that space.
    let about = &alert.about;
    let pending: Option<(i64, bool)> = tx
        .query_row(
            "SELECT id,wakes FROM alerts
                WHERE recipient_id=?1 AND read=0 AND message=?2 AND attachments=?3
                    AND space IS ?4 AND space_id IS ?5 AND kind=?6 AND sender=?7",
            params![
                space_id,
                alert.message,
                attachments,
                about.name,
                about.id,
                alert.kind,
                alert.sender
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((id, woke)) = pending {
        if woke || !wakes {
            return Ok(Queued { id, new: false });
        }
        tx.execute("UPDATE alerts SET wakes=1 WHERE id=?1", [id])?;
        return Ok(Queued { id, new: true });
    }
    let unread: i64 = tx.query_row(
        "SELECT COUNT(*) FROM alerts WHERE recipient_id=?1 AND read=0",
        [space_id],
        |row| row.get(0),
    )?;
    ensure!(
        unread < MAX_UNREAD,
        "the space already has {MAX_UNREAD} unread alerts"
    );
    let id = alerts::insert(tx, Some(space_id), alert, wakes, for_user)?;
    if delivered_bytes(tx, space_id)? > MAX_UNREAD_BYTES {
        tx.execute("DELETE FROM alerts WHERE id=?1", [id])?;
        bail!("the space's unread alerts would exceed {MAX_UNREAD_BYTES} bytes");
    }
    Ok(Queued { id, new: true })
}

/// The bytes the unread alerts of the space `space_id` take as its agent
/// receives them.
fn delivered_bytes(db: &Connection, space_id: &SpaceId) -> Result<usize> {
    Ok(serde_json::to_vec(&unread(db, space_id)?.alerts)?.len())
}

/// The unread alerts of the space `space_id`, oldest first, as its agent
/// reads them.
pub(super) fn unread(db: &Connection, space_id: &SpaceId) -> Result<Unread> {
    let mut statement = db.prepare(&format!(
        "SELECT {COLUMNS},a.wakes FROM {FROM}
            WHERE a.recipient_id=?1 AND a.read=0 ORDER BY a.id"
    ))?;
    let mut rows = statement.query([space_id])?;
    let mut unread = Unread::default();
    while let Some(row) = rows.next()? {
        unread.alerts.push(alerts::row(row)?);
        unread.wakes |= row.get::<_, bool>("wakes")?;
    }
    Ok(unread)
}

/// Every space's unread alerts, oldest first, as its agent reads them.
pub(crate) fn every_unread(db: &Connection) -> Result<Vec<Alert>> {
    Ok(db
        .prepare(&format!(
            "SELECT {COLUMNS} FROM {FROM}
                WHERE a.recipient_id IS NOT NULL AND a.read=0 ORDER BY a.id"
        ))?
        .query_map([], alerts::row)?
        .collect::<rusqlite::Result<_>>()?)
}

/// Mark the alert `id` of the space `space_id` read, which its agent
/// received, and delete the question whose answer it carried.
pub(super) fn mark_delivered(tx: &Transaction<'_>, space_id: &SpaceId, id: i64) -> Result<()> {
    tx.execute(
        "UPDATE alerts SET read=1 WHERE id=?1 AND recipient_id=?2",
        params![id, space_id],
    )?;
    crate::daemon::questions::delete_delivered(tx, space_id, id)
}

/// Give the user the unread alerts of the space `space_id`, which is being
/// removed, that the user would have received without its agent, with the
/// files they attach, in the transaction that deletes the space and with it
/// the alerts. Returns the last alert recorded.
pub(crate) fn hand_to_user(tx: &Transaction<'_>, space_id: &SpaceId) -> Result<Option<i64>> {
    let unread = tx
        .prepare(
            "SELECT kind,sender,space,space_id,message,attachments FROM alerts
                WHERE recipient_id=?1 AND read=0 AND for_user=1 ORDER BY id",
        )?
        .query_map([space_id], |row| {
            Ok(NewAlert {
                kind: row.get("kind")?,
                sender: row.get("sender")?,
                about: About {
                    name: row.get("space")?,
                    id: row.get("space_id")?,
                    subject: None,
                },
                message: row.get("message")?,
                attachments: crate::daemon::store::json_column(row, "attachments")?
                    .unwrap_or_default(),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut last = None;
    for alert in unread {
        let recorded = notifications::insert(tx, &alert)?;
        if recorded.new {
            last = Some(recorded.id);
        }
    }
    Ok(last)
}
