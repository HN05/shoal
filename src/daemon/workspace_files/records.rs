//! The store's records of attached files: which copy belongs to which
//! workspace or chat, how it came, and the removed workspaces and ended
//! chats whose files stay.
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::{AttachedFile, Attacher, FileSource, Owner, OwnerKind, layout::Copy};

const COLUMNS: &str = "id,owner_kind,owner_name,source,name,title,path,bytes,created_at,
    attached_by,COALESCE(attached_files.expires_at,
        (SELECT r.expires_at FROM removed_workspaces r
            WHERE r.workspace_id=attached_files.owner_id AND attached_files.owner_kind='workspace'
                AND attached_files.owner_id NOT IN (SELECT id FROM workspaces)))";

/// The columns [`owner`] reads.
const OWNER_COLUMNS: &str = "owner_kind,owner_id,owner_name,repository_id,parent_id";

/// Whether a file's owner is gone: a removed workspace, which keeps its
/// results, or an ended chat.
const GONE: &str = "(CASE owner_kind WHEN 'chat' THEN owner_id NOT IN (SELECT id FROM chats)
    ELSE owner_id NOT IN (SELECT id FROM workspaces) END)";

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AttachedFile> {
    let (kind, owner) = (row.get(1)?, row.get(2)?);
    let (workspace, chat) = match kind {
        OwnerKind::Chat => (None, Some(owner)),
        OwnerKind::Workspace => (Some(owner), None),
    };
    Ok(AttachedFile {
        id: row.get(0)?,
        workspace,
        chat,
        source: row.get(3)?,
        name: row.get(4)?,
        title: row.get(5)?,
        path: PathBuf::from(row.get::<_, String>(6)?),
        bytes: row.get::<_, i64>(7)?.try_into().unwrap_or_default(),
        created_at: row.get(8)?,
        attached_by: row
            .get::<_, Option<String>>(9)?
            .map(|json| serde_json::from_str(&json))
            .transpose()
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    9,
                    rusqlite::types::Type::Text,
                    error.into(),
                )
            })?,
        expires_at: row.get(10)?,
    })
}

fn owner(row: &rusqlite::Row<'_>) -> rusqlite::Result<Owner> {
    Ok(Owner {
        kind: row.get(0)?,
        id: row.get(1)?,
        name: row.get(2)?,
        repository_id: row.get(3)?,
        parent_id: row.get(4)?,
    })
}

/// Record the `copies` as attached to the workspace or chat `owner_id` of
/// `kind` from `source` by `attacher`, unknown when none, in the caller's
/// transaction. A copy already recorded, as when the same files are sent
/// again, keeps its record and takes any new title.
pub(crate) fn record(
    tx: &Connection,
    kind: OwnerKind,
    owner_id: &str,
    source: FileSource,
    attacher: Option<&Attacher>,
    copies: &[Copy],
) -> Result<()> {
    let attacher = attacher.map(serde_json::to_string).transpose()?;
    let owners = match kind {
        OwnerKind::Chat => "SELECT id,name,NULL AS parent_id,repository_id FROM chats",
        OwnerKind::Workspace => {
            "SELECT id,name,parent_workspace_id AS parent_id,repository_id FROM workspaces"
        }
    };
    for copy in copies {
        let name = copy
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        tx.execute(
            &format!(
                "INSERT INTO attached_files
                    (owner_kind,owner_id,owner_name,parent_id,repository_id,source,name,title,
                        path,bytes,created_at,attached_by)
                    SELECT ?8,id,name,parent_id,repository_id,?2,?3,?4,?5,?6,unixepoch(),?7
                    FROM ({owners}) WHERE id=?1
                    ON CONFLICT(path) DO UPDATE SET title=COALESCE(excluded.title,title)"
            ),
            params![
                owner_id,
                source,
                name,
                copy.title,
                copy.path.to_string_lossy(),
                i64::try_from(copy.bytes)?,
                attacher,
                kind
            ],
        )?;
    }
    Ok(())
}

/// [`record`] in a transaction of its own, returning the copies' records in
/// their order.
pub(super) fn record_all(
    db: &mut Connection,
    owner: (OwnerKind, &str),
    source: FileSource,
    attacher: &Attacher,
    copies: &[Copy],
) -> Result<Vec<AttachedFile>> {
    let (kind, owner_id) = owner;
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    record(&tx, kind, owner_id, source, Some(attacher), copies)?;
    let records = {
        let mut statement = tx.prepare(&format!(
            "SELECT {COLUMNS} FROM attached_files WHERE path=?1"
        ))?;
        copies
            .iter()
            .map(|copy| {
                statement
                    .query_row([copy.path.to_string_lossy()], row)
                    .optional()?
                    // An owner that went meanwhile records nothing.
                    .with_context(|| format!("the {kind} is gone"))
            })
            .collect::<Result<Vec<_>>>()?
    };
    tx.commit()?;
    Ok(records)
}

/// The files of the workspace or chat `owner_id`, oldest first.
pub(super) fn list(db: &Connection, owner_id: &str) -> Result<Vec<AttachedFile>> {
    Ok(db
        .prepare(&format!(
            "SELECT {COLUMNS} FROM attached_files WHERE owner_id=?1 ORDER BY id"
        ))?
        .query_map([owner_id], row)?
        .collect::<rusqlite::Result<_>>()?)
}

/// The files of repository `repository_id` whose workspace has no recorded
/// execution, running or unresolved, or whose chat has ended, oldest first.
pub(super) fn idle(db: &Connection, repository_id: &str) -> Result<Vec<AttachedFile>> {
    Ok(db
        .prepare(&format!(
            "SELECT {COLUMNS} FROM attached_files WHERE repository_id=?1
                AND owner_id NOT IN (SELECT workspace_id FROM executions)
                AND owner_id NOT IN (SELECT id FROM chats)
                ORDER BY id"
        ))?
        .query_map([repository_id], row)?
        .collect::<rusqlite::Result<_>>()?)
}

/// The latest removed workspace or ended chat with that name or ID whose
/// files are kept.
pub(super) fn gone_owner(db: &Connection, selector: &str) -> Result<Option<Owner>> {
    Ok(db
        .query_row(
            &format!(
                "SELECT {OWNER_COLUMNS} FROM attached_files
                    WHERE (owner_id=?1 OR owner_name=?1) AND {GONE}
                    ORDER BY id DESC LIMIT 1"
            ),
            [selector],
            owner,
        )
        .optional()?)
}

/// The removed workspaces and ended chats whose files are kept, those that
/// kept them first oldest first.
pub(super) fn kept(db: &Connection) -> Result<Vec<Owner>> {
    Ok(db
        .prepare(&format!(
            "SELECT {OWNER_COLUMNS} FROM attached_files WHERE {GONE}
                GROUP BY owner_id ORDER BY MIN(id)"
        ))?
        .query_map([], owner)?
        .collect::<rusqlite::Result<_>>()?)
}

/// The owners in repository `repository_id` whose kept files are due: the
/// removed workspaces whose removal record expired, or that never had one,
/// and the ended chats whose files expired.
pub(super) fn expired(db: &Connection, repository_id: &str) -> Result<Vec<Owner>> {
    Ok(db
        .prepare(&format!(
            "SELECT {OWNER_COLUMNS} FROM attached_files WHERE repository_id=?1 AND {GONE}
                AND CASE owner_kind
                    WHEN 'chat' THEN expires_at<=unixepoch()
                    ELSE owner_id NOT IN (SELECT workspace_id FROM removed_workspaces) END
                GROUP BY owner_id"
        ))?
        .query_map([repository_id], owner)?
        .collect::<rusqlite::Result<_>>()?)
}

/// Give the files of repository `repository_id`'s ended chats that have no
/// time to go yet the time `expires_at`.
pub(super) fn stamp_ended_chats(
    db: &Connection,
    repository_id: &str,
    expires_at: i64,
) -> Result<()> {
    db.execute(
        "UPDATE attached_files SET expires_at=?2
            WHERE repository_id=?1 AND owner_kind='chat' AND expires_at IS NULL
                AND owner_id NOT IN (SELECT id FROM chats)",
        params![repository_id, expires_at],
    )?;
    Ok(())
}

/// Give the files of the chat `chat_id` its new name `name`, in the
/// transaction that renames it.
pub(crate) fn rename_chat(tx: &Transaction<'_>, chat_id: &str, name: &str) -> Result<()> {
    tx.execute(
        "UPDATE attached_files SET owner_name=?2 WHERE owner_kind='chat' AND owner_id=?1",
        params![chat_id, name],
    )?;
    Ok(())
}

/// Forget the records `ids`.
pub(super) fn forget(db: &mut Connection, ids: &[i64]) -> Result<()> {
    let tx = db.transaction()?;
    for id in ids {
        tx.execute("DELETE FROM attached_files WHERE id=?1", [id])?;
    }
    tx.commit()?;
    Ok(())
}

/// Forget every record of the workspace or chat `owner_id`.
pub(super) fn forget_owner(db: &Connection, owner_id: &str) -> Result<()> {
    db.execute("DELETE FROM attached_files WHERE owner_id=?1", [owner_id])?;
    Ok(())
}

/// Forget the records of the workspace `workspace_id` but its results,
/// which keep its last name, and move the results it was the parent of to
/// its own parent, in the transaction that deletes it, before its row goes.
pub(crate) fn forget_removed(tx: &Transaction<'_>, workspace_id: &str) -> Result<()> {
    tx.execute(
        "DELETE FROM attached_files WHERE owner_id=?1 AND source!=?2",
        params![workspace_id, FileSource::Result],
    )?;
    tx.execute(
        "UPDATE attached_files SET owner_name=(SELECT name FROM workspaces WHERE id=?1)
            WHERE owner_id=?1",
        [workspace_id],
    )?;
    tx.execute(
        "UPDATE attached_files
            SET parent_id=(SELECT parent_workspace_id FROM workspaces WHERE id=?1)
            WHERE parent_id=?1",
        [workspace_id],
    )?;
    Ok(())
}

/// Forget every record of repository `repository_id`.
pub(super) fn forget_repository(db: &Connection, repository_id: &str) -> Result<()> {
    db.execute(
        "DELETE FROM attached_files WHERE repository_id=?1",
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
        .prepare("SELECT id,path FROM attached_files WHERE repository_id=?1")?
        .query_map([repository_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (id, path) in recorded {
        if let Ok(inside) = Path::new(&path).strip_prefix(source) {
            tx.execute(
                "UPDATE attached_files SET path=?2 WHERE id=?1",
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
