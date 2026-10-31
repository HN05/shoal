//! The store's records of attached files: which copy belongs to which
//! workspace or chat, how it came, and the removed workspaces and ended chats
//! whose files stay. A space is gone when neither a workspace nor a chat has
//! its ID; its removal or end record says how long its files stay.
use std::path::{Path, PathBuf};

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::{AttachedFile, Attacher, FileSource, Space, layout::Copy};

const COLUMNS: &str = "id,space,source,name,title,path,bytes,created_at,attached_by,
    COALESCE((SELECT expires_at FROM removed_workspaces r
            WHERE r.workspace_id=workspace_files.space_id
                AND workspace_files.space_id NOT IN (SELECT id FROM workspaces)),
        (SELECT expires_at FROM ended_chats e WHERE e.chat_id=workspace_files.space_id)),
    space_id IN (SELECT id FROM chats) OR space_id IN (SELECT chat_id FROM ended_chats)";

/// The owners of attached files that are gone.
const GONE: &str = "space_id NOT IN (SELECT id FROM workspaces)
    AND space_id NOT IN (SELECT id FROM chats)";

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AttachedFile> {
    let space: String = row.get(1)?;
    let (workspace, chat) = match row.get(10)? {
        true => (None, Some(space)),
        false => (Some(space), None),
    };
    Ok(AttachedFile {
        id: row.get(0)?,
        workspace,
        chat,
        source: row.get(2)?,
        name: row.get(3)?,
        title: row.get(4)?,
        path: PathBuf::from(row.get::<_, String>(5)?),
        bytes: row.get::<_, i64>(6)?.try_into().unwrap_or_default(),
        created_at: row.get(7)?,
        attached_by: row
            .get::<_, Option<String>>(8)?
            .map(|json| serde_json::from_str(&json))
            .transpose()
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    8,
                    rusqlite::types::Type::Text,
                    error.into(),
                )
            })?,
        expires_at: row.get(9)?,
    })
}

fn space(row: &rusqlite::Row<'_>) -> rusqlite::Result<Space> {
    Ok(Space {
        id: row.get(0)?,
        name: row.get(1)?,
        repository_id: row.get(2)?,
        parent_id: row.get(3)?,
    })
}

/// Record the `copies` as attached to the space `space_id` from
/// `source` by `attacher`, unknown when none, in the caller's transaction. A
/// copy already recorded, as when the same files are sent again, keeps its
/// record and takes any new title; none is recorded for a space that is gone.
pub(crate) fn record(
    tx: &Connection,
    space_id: &str,
    source: FileSource,
    attacher: Option<&Attacher>,
    copies: &[Copy],
) -> Result<()> {
    let attacher = attacher.map(serde_json::to_string).transpose()?;
    for copy in copies {
        let name = copy
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        tx.execute(
            "INSERT INTO workspace_files
                (space_id,space,parent_id,repository_id,source,name,title,path,bytes,
                    created_at,attached_by)
                SELECT id,name,parent,repository_id,?2,?3,?4,?5,?6,unixepoch(),?7
                FROM (SELECT id,name,parent_workspace_id AS parent,repository_id
                        FROM workspaces WHERE id=?1
                    UNION ALL SELECT id,name,NULL,repository_id FROM chats WHERE id=?1)
                WHERE true
                ON CONFLICT(path) DO UPDATE SET title=COALESCE(excluded.title,title)",
            params![
                space_id,
                source,
                name,
                copy.title,
                copy.path.to_string_lossy(),
                i64::try_from(copy.bytes)?,
                attacher
            ],
        )?;
    }
    Ok(())
}

/// [`record`] in a transaction of its own, returning the copies' records in
/// their order.
pub(super) fn record_all(
    db: &mut Connection,
    space_id: &str,
    source: FileSource,
    attacher: &Attacher,
    copies: &[Copy],
) -> Result<Vec<AttachedFile>> {
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    record(&tx, space_id, source, Some(attacher), copies)?;
    let records = {
        let mut statement = tx.prepare(&format!(
            "SELECT {COLUMNS} FROM workspace_files WHERE path=?1"
        ))?;
        copies
            .iter()
            .map(|copy| Ok(statement.query_row([copy.path.to_string_lossy()], row)?))
            .collect::<Result<Vec<_>>>()?
    };
    tx.commit()?;
    Ok(records)
}

/// The files of the space `space_id`, oldest first.
pub(super) fn list(db: &Connection, space_id: &str) -> Result<Vec<AttachedFile>> {
    Ok(db
        .prepare(&format!(
            "SELECT {COLUMNS} FROM workspace_files WHERE space_id=?1 ORDER BY id"
        ))?
        .query_map([space_id], row)?
        .collect::<rusqlite::Result<_>>()?)
}

/// The files of repository `repository_id` whose workspace has no recorded
/// execution, running or unresolved, and that no running chat owns, oldest
/// first.
pub(super) fn idle(db: &Connection, repository_id: &str) -> Result<Vec<AttachedFile>> {
    Ok(db
        .prepare(&format!(
            "SELECT {COLUMNS} FROM workspace_files WHERE repository_id=?1
                AND space_id NOT IN (SELECT workspace_id FROM executions)
                AND space_id NOT IN (SELECT id FROM chats)
                ORDER BY id"
        ))?
        .query_map([repository_id], row)?
        .collect::<rusqlite::Result<_>>()?)
}

/// The latest removed workspace or ended chat with that name or ID whose
/// files are kept.
pub(super) fn gone_space(db: &Connection, selector: &str) -> Result<Option<Space>> {
    Ok(db
        .query_row(
            &format!(
                "SELECT space_id,space,repository_id,parent_id FROM workspace_files
                    WHERE (space_id=?1 OR space=?1) AND {GONE}
                    ORDER BY id DESC LIMIT 1"
            ),
            [selector],
            space,
        )
        .optional()?)
}

/// The removed workspaces and ended chats whose files are kept, those that
/// attached them first oldest first.
pub(super) fn kept(db: &Connection) -> Result<Vec<Space>> {
    Ok(db
        .prepare(&format!(
            "SELECT space_id,space,repository_id,parent_id FROM workspace_files
                WHERE {GONE} GROUP BY space_id ORDER BY MIN(id)"
        ))?
        .query_map([], space)?
        .collect::<rusqlite::Result<_>>()?)
}

/// The removed workspaces and ended chats of repository `repository_id`
/// whose files are kept though their record expired, or that never had one.
/// An ended chat's record stays until its files are deleted, so a failed
/// deletion still lists them as the chat's.
pub(super) fn expired(db: &Connection, repository_id: &str) -> Result<Vec<String>> {
    Ok(db
        .prepare(&format!(
            "SELECT DISTINCT space_id FROM workspace_files WHERE repository_id=?1 AND {GONE}
                AND space_id NOT IN (SELECT workspace_id FROM removed_workspaces)
                AND space_id NOT IN (SELECT chat_id FROM ended_chats
                    WHERE expires_at > unixepoch())"
        ))?
        .query_map([repository_id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Forget the records `ids`.
pub(super) fn forget(db: &mut Connection, ids: &[i64]) -> Result<()> {
    let tx = db.transaction()?;
    for id in ids {
        tx.execute("DELETE FROM workspace_files WHERE id=?1", [id])?;
    }
    tx.commit()?;
    Ok(())
}

/// Forget every record of the removed workspace or ended chat `space_id`.
pub(super) fn forget_space(db: &mut Connection, space_id: &str) -> Result<()> {
    let tx = db.transaction()?;
    tx.execute("DELETE FROM workspace_files WHERE space_id=?1", [space_id])?;
    tx.execute("DELETE FROM ended_chats WHERE chat_id=?1", [space_id])?;
    tx.commit()?;
    Ok(())
}

/// Keep the files of the chat `chat_id`, which keep its last name, until
/// `expires_at`, in the transaction that deletes the chat, before its row
/// goes.
pub(crate) fn keep_ended_chat(tx: &Transaction<'_>, chat_id: &str, expires_at: i64) -> Result<()> {
    tx.execute(
        "UPDATE workspace_files SET space=(SELECT name FROM chats WHERE id=?1)
            WHERE space_id=?1",
        [chat_id],
    )?;
    tx.execute(
        "INSERT OR REPLACE INTO ended_chats(chat_id,expires_at) SELECT ?1,?2
            WHERE EXISTS(SELECT 1 FROM workspace_files WHERE space_id=?1)",
        params![chat_id, expires_at],
    )?;
    Ok(())
}

/// Forget the expired records of ended chats whose files are gone, as when
/// `files rm` deleted them all.
pub(super) fn forget_expired_chats(db: &mut Connection) -> Result<()> {
    db.execute(
        "DELETE FROM ended_chats WHERE expires_at <= unixepoch()
            AND chat_id NOT IN (SELECT space_id FROM workspace_files)",
        [],
    )?;
    Ok(())
}

/// Forget the records of the workspace `workspace_id` but its results,
/// which keep its last name, and move the results it was the parent of to
/// its own parent, in the transaction that deletes it, before its row goes.
pub(crate) fn forget_removed(tx: &Transaction<'_>, workspace_id: &str) -> Result<()> {
    tx.execute(
        "DELETE FROM workspace_files WHERE space_id=?1 AND source!=?2",
        params![workspace_id, FileSource::Result],
    )?;
    tx.execute(
        "UPDATE workspace_files SET space=(SELECT name FROM workspaces WHERE id=?1)
            WHERE space_id=?1",
        [workspace_id],
    )?;
    tx.execute(
        "UPDATE workspace_files
            SET parent_id=(SELECT parent_workspace_id FROM workspaces WHERE id=?1)
            WHERE parent_id=?1",
        [workspace_id],
    )?;
    Ok(())
}

/// Forget every record of repository `repository_id`.
pub(super) fn forget_repository(db: &Connection, repository_id: &str) -> Result<()> {
    db.execute(
        "DELETE FROM workspace_files WHERE repository_id=?1",
        [repository_id],
    )?;
    Ok(())
}

/// Point the recorded copies of repository `repository_id` from `source` to
/// `destination`, in the transaction that moves the repository's directory.
pub(crate) fn rebase(
    tx: &Transaction<'_>,
    repository_id: &str,
    source: &Path,
    destination: &Path,
) -> Result<()> {
    let recorded: Vec<(i64, String)> = tx
        .prepare("SELECT id,path FROM workspace_files WHERE repository_id=?1")?
        .query_map([repository_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (id, path) in recorded {
        if let Ok(inside) = Path::new(&path).strip_prefix(source) {
            tx.execute(
                "UPDATE workspace_files SET path=?2 WHERE id=?1",
                params![id, destination.join(inside).to_string_lossy()],
            )?;
        }
    }
    Ok(())
}

/// The repositories whose earlier copies are still to be recorded.
pub(super) fn earlier_pending(db: &Connection) -> Result<Vec<String>> {
    Ok(db
        .prepare("SELECT repository_id FROM earlier_attached_files")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Whether a move of repository `repository_id`'s directory is unfinished.
pub(super) fn moving(db: &Connection, repository_id: &str) -> Result<bool> {
    crate::daemon::store::exists(
        db,
        "SELECT 1 FROM repository_moves WHERE repository_id=?1",
        [repository_id],
    )
}

/// The workspaces of repository `repository_id`.
pub(super) fn repository_workspaces(db: &Connection, repository_id: &str) -> Result<Vec<String>> {
    Ok(db
        .prepare("SELECT id FROM workspaces WHERE repository_id=?1")?
        .query_map([repository_id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Mark repository `repository_id`'s earlier copies recorded.
pub(super) fn earlier_recorded(db: &Connection, repository_id: &str) -> Result<()> {
    db.execute(
        "DELETE FROM earlier_attached_files WHERE repository_id=?1",
        [repository_id],
    )?;
    Ok(())
}
