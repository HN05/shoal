//! The store's records of attached files: which copy belongs to which
//! workspace, how it came, and the removed workspaces whose results stay.
use std::path::{Path, PathBuf};

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::{AttachedFile, Attacher, FileSource, Owner, layout::Copy};

const COLUMNS: &str = "id,owner,source,name,title,path,bytes,created_at,attached_by,
    (SELECT expires_at FROM removed_workspaces r WHERE r.workspace_id=workspace_files.owner_id
        AND workspace_files.owner_id NOT IN (SELECT id FROM workspaces))";

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AttachedFile> {
    Ok(AttachedFile {
        id: row.get(0)?,
        workspace: row.get(1)?,
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

fn owner(row: &rusqlite::Row<'_>) -> rusqlite::Result<Owner> {
    Ok(Owner {
        id: row.get(0)?,
        name: row.get(1)?,
        repository_id: row.get(2)?,
        parent_id: row.get(3)?,
    })
}

/// Record the `copies` as attached to the workspace `workspace_id` from
/// `source` by `attacher`, unknown when none, in the caller's transaction. A
/// copy already recorded, as when the same files are sent again, keeps its
/// record and takes any new title.
pub(crate) fn record(
    tx: &Connection,
    workspace_id: &str,
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
                (owner_id,owner,parent_id,repository_id,source,name,title,path,bytes,
                    created_at,attached_by)
                SELECT id,name,parent_workspace_id,repository_id,?2,?3,?4,?5,?6,unixepoch(),?7
                FROM workspaces WHERE id=?1
                ON CONFLICT(path) DO UPDATE SET title=COALESCE(excluded.title,title)",
            params![
                workspace_id,
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
    workspace_id: &str,
    source: FileSource,
    attacher: &Attacher,
    copies: &[Copy],
) -> Result<Vec<AttachedFile>> {
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    record(&tx, workspace_id, source, Some(attacher), copies)?;
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

/// The workspace `workspace_id`'s files, oldest first.
pub(super) fn list(db: &Connection, workspace_id: &str) -> Result<Vec<AttachedFile>> {
    Ok(db
        .prepare(&format!(
            "SELECT {COLUMNS} FROM workspace_files WHERE owner_id=?1 ORDER BY id"
        ))?
        .query_map([workspace_id], row)?
        .collect::<rusqlite::Result<_>>()?)
}

/// The files of repository `repository_id` whose workspace has no recorded
/// execution, running or unresolved, oldest first.
pub(super) fn idle(db: &Connection, repository_id: &str) -> Result<Vec<AttachedFile>> {
    Ok(db
        .prepare(&format!(
            "SELECT {COLUMNS} FROM workspace_files WHERE repository_id=?1
                AND owner_id NOT IN (SELECT workspace_id FROM executions)
                ORDER BY id"
        ))?
        .query_map([repository_id], row)?
        .collect::<rusqlite::Result<_>>()?)
}

/// The latest removed workspace with that name or ID whose files are kept.
pub(super) fn removed_owner(db: &Connection, selector: &str) -> Result<Option<Owner>> {
    Ok(db
        .query_row(
            "SELECT owner_id,owner,repository_id,parent_id FROM workspace_files
                WHERE (owner_id=?1 OR owner=?1)
                    AND owner_id NOT IN (SELECT id FROM workspaces)
                ORDER BY id DESC LIMIT 1",
            [selector],
            owner,
        )
        .optional()?)
}

/// The removed workspaces whose results are kept, those that handed them
/// back first oldest first.
pub(super) fn kept(db: &Connection) -> Result<Vec<Owner>> {
    Ok(db
        .prepare(
            "SELECT owner_id,owner,repository_id,parent_id FROM workspace_files
                WHERE owner_id NOT IN (SELECT id FROM workspaces)
                GROUP BY owner_id ORDER BY MIN(id)",
        )?
        .query_map([], owner)?
        .collect::<rusqlite::Result<_>>()?)
}

/// The removed workspaces of repository `repository_id` whose results are
/// kept though their removal record expired, or that never had one.
pub(super) fn expired(db: &Connection, repository_id: &str) -> Result<Vec<String>> {
    Ok(db
        .prepare(
            "SELECT DISTINCT owner_id FROM workspace_files WHERE repository_id=?1
                AND owner_id NOT IN (SELECT id FROM workspaces)
                AND owner_id NOT IN (SELECT workspace_id FROM removed_workspaces)",
        )?
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

/// Forget every record of the workspace `workspace_id`.
pub(super) fn forget_workspace(db: &Connection, workspace_id: &str) -> Result<()> {
    db.execute(
        "DELETE FROM workspace_files WHERE owner_id=?1",
        [workspace_id],
    )?;
    Ok(())
}

/// Forget the records of the workspace `workspace_id` but its results,
/// which keep its last name, and move the results it was the parent of to
/// its own parent, in the transaction that deletes it, before its row goes.
pub(crate) fn forget_removed(tx: &Transaction<'_>, workspace_id: &str) -> Result<()> {
    tx.execute(
        "DELETE FROM workspace_files WHERE owner_id=?1 AND source!=?2",
        params![workspace_id, FileSource::Result],
    )?;
    tx.execute(
        "UPDATE workspace_files SET owner=(SELECT name FROM workspaces WHERE id=?1)
            WHERE owner_id=?1",
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
