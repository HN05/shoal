//! The `space_messages` rows: queueing a message for a space's agent,
//! listing the unread ones as the agent reads them, deleting delivered
//! ones, and what becomes of them when their files move or their space
//! goes.
use std::path::{Path, PathBuf};

use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::{About, AgentMessage, Delivery, MAX_UNREAD, MAX_UNREAD_BYTES, Unread};
use crate::{
    client::{
        space_ids::{SpaceId, SpaceNumber},
        space_names::{SpaceName, label},
    },
    daemon::{
        notifications::{self, NotificationKind},
        workspace_files,
    },
    state::WorkspaceState,
};

/// Queue `message` with the files `attachments` Shoal has copied for the
/// agent of the space `space_id`, `about` another space when it concerns
/// one, in the caller's transaction: its ID, or `None` when an identical
/// message about the same space is still unread. An identical unread
/// message that does not wake the agent wakes it once queued again with
/// [`Delivery::Wake`]. A workspace being removed takes none, since removal
/// decides from the messages it has.
pub(super) fn insert(
    tx: &Transaction<'_>,
    space_id: &SpaceId,
    about: Option<&About>,
    message: &str,
    attachments: &[PathBuf],
    delivery: Delivery,
) -> Result<Option<i64>> {
    let attachments = serde_json::to_string(attachments)?;
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
    // Names repeat: a message is about the space its ID names, and is the
    // same as another only when both are about that space.
    let (about_name, about_id, kind) = match about {
        Some(about) => (Some(about.name.as_str()), about.id.as_ref(), about.kind),
        None => (None, None, None),
    };
    let pending: Option<(i64, bool)> = tx
        .query_row(
            "SELECT id,wakes FROM space_messages
                WHERE space_id=?1 AND message=?2 AND attachments=?3 AND about IS ?4
                    AND about_id IS ?5",
            params![space_id, message, attachments, about_name, about_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((id, woke)) = pending {
        if woke || !wakes {
            return Ok(None);
        }
        tx.execute("UPDATE space_messages SET wakes=1 WHERE id=?1", [id])?;
        return Ok(Some(id));
    }
    let (unread, bytes): (i64, i64) = tx.query_row(
        "SELECT COUNT(*),
                COALESCE(SUM(LENGTH(CAST(COALESCE(about,'') || message || attachments AS BLOB))),0)
            FROM space_messages WHERE space_id=?1",
        [space_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    ensure!(
        unread < MAX_UNREAD,
        "the space already has {MAX_UNREAD} unread agent messages"
    );
    let size = about_name.map_or(0, str::len) + message.len() + attachments.len();
    ensure!(
        usize::try_from(bytes)? + size <= MAX_UNREAD_BYTES,
        "the space's unread agent messages would exceed {MAX_UNREAD_BYTES} bytes"
    );
    tx.execute(
        "INSERT INTO space_messages
            (space_id,created_at,message,attachments,wakes,about,about_id,kind)
            VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            space_id,
            crate::time::UnixTime::now(),
            message,
            attachments,
            wakes,
            about_name,
            about_id,
            kind
        ],
    )?;
    Ok(Some(tx.last_insert_rowid()))
}

/// The unread messages of the space `space_id`, oldest first, as its agent
/// reads them.
pub(super) fn unread(db: &Connection, space_id: &SpaceId) -> Result<Unread> {
    let mut unread = Unread::default();
    for (_, message, wakes) in listed(db, Some(space_id))? {
        unread.messages.push(message);
        unread.wakes |= wakes;
    }
    Ok(unread)
}

/// Every space's unread messages, oldest first, with their space's ID, as
/// its agent reads them.
pub(crate) fn every_unread(db: &Connection) -> Result<Vec<(SpaceId, AgentMessage)>> {
    Ok(listed(db, None)?
        .into_iter()
        .map(|(space_id, message, _)| (space_id, message))
        .collect())
}

/// The unread messages of the space `space_id`, or of every space, oldest
/// first, each with its space's ID and whether it wakes an idle agent.
fn listed(
    db: &Connection,
    space_id: Option<&SpaceId>,
) -> Result<Vec<(SpaceId, AgentMessage, bool)>> {
    let filter = match space_id {
        Some(_) => "WHERE message.space_id=?1",
        None => "",
    };
    let mut statement = db.prepare(&format!(
        "SELECT message.space_id,message.id,message.created_at,message.message,
                message.attachments,message.wakes,message.about,space.number
            FROM space_messages message
            LEFT JOIN spaces space ON space.id=message.about_id
            {filter} ORDER BY message.id"
    ))?;
    let mut rows = match space_id {
        Some(space_id) => statement.query([space_id])?,
        None => statement.query([])?,
    };
    let mut messages = Vec::new();
    while let Some(row) = rows.next()? {
        let message = AgentMessage {
            id: row.get("id")?,
            created_at: row.get("created_at")?,
            message: rendered(row.get("about")?, row.get("number")?, row.get("message")?),
            attachments: crate::daemon::store::json_column(row, "attachments")?.unwrap_or_default(),
        };
        messages.push((row.get("space_id")?, message, row.get("wakes")?));
    }
    Ok(messages)
}

/// `message` as an agent reads it: naming the space it is `about`, by its
/// `number` while it has one, since names repeat.
fn rendered(about: Option<SpaceName>, number: Option<SpaceNumber>, message: String) -> String {
    match (about, number) {
        (None, _) => message,
        (Some(about), None) => format!("{about}: {message}"),
        (Some(about), Some(number)) => format!("{}: {message}", label(number, &about)),
    }
}

/// Delete the message `id` of the space `space_id`, which its agent
/// received, with the question whose answer it carried.
pub(super) fn delete_delivered(tx: &Transaction<'_>, space_id: &SpaceId, id: i64) -> Result<()> {
    tx.execute(
        "DELETE FROM space_messages WHERE id=?1 AND space_id=?2",
        params![id, space_id],
    )?;
    crate::daemon::questions::delete_delivered(tx, space_id, id)
}

/// Point the attached paths of every space's messages from `source` to
/// `destination`, in the transaction that moves a repository's directory
/// there. Any space may hold the files a space of that repository sent,
/// such as a chat of another repository.
pub(crate) fn rebase_attachments(
    tx: &Transaction<'_>,
    source: &Path,
    destination: &Path,
) -> Result<()> {
    let attached: Vec<(i64, String)> = tx
        .prepare("SELECT id,attachments FROM space_messages WHERE attachments!='[]'")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (id, attachments) in attached {
        let paths: Vec<PathBuf> = serde_json::from_str::<Vec<PathBuf>>(&attachments)?
            .into_iter()
            .map(|path| match path.strip_prefix(source) {
                Ok(inside) => destination.join(inside),
                Err(_) => path,
            })
            .collect();
        tx.execute(
            "UPDATE space_messages SET attachments=?2 WHERE id=?1",
            params![id, serde_json::to_string(&paths)?],
        )?;
    }
    Ok(())
}

/// Give the user the notifications that the unread messages of the space
/// `space_id`, which is being removed, stand for, with the files they
/// attach, in the transaction that deletes the space and with it the
/// messages. Returns the last notification recorded.
pub(crate) fn hand_to_user(tx: &Transaction<'_>, space_id: &SpaceId) -> Result<Option<i64>> {
    let unread = tx
        .prepare(
            "SELECT about,about_id,kind,message,attachments FROM space_messages
                WHERE space_id=?1 AND kind IS NOT NULL ORDER BY id",
        )?
        .query_map([space_id], |row| {
            let about = notifications::About {
                name: row.get(0)?,
                id: row.get(1)?,
                subject: None,
            };
            let attachments: Vec<PathBuf> =
                crate::daemon::store::json_column(row, "attachments")?.unwrap_or_default();
            Ok((about, row.get(2)?, row.get(3)?, attachments))
        })?
        .collect::<rusqlite::Result<Vec<(notifications::About, NotificationKind, String, Vec<PathBuf>)>>>()?;
    let mut last = None;
    for (about, kind, message, attachments) in unread {
        let files = workspace_files::ids_at(tx, &attachments)?;
        let recorded = notifications::insert(tx, &about, kind, &message, &files)?;
        if recorded.new {
            last = Some(recorded.id);
        }
    }
    Ok(last)
}
