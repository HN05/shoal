//! The store's records of attached files. A space is gone when no space
//! has its ID; its row in `removed_space_files` says how long its files
//! stay.
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use std::path::{Path, PathBuf};

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::{
    AttachedFile, AttachedTo, Attacher, FileSource, HomeKey, KeptCopy, Space, layout::Copy,
};
use shoal_protocol::{files::SentFile, model::SpaceKind};

const COLUMNS: &str = "id,space,source,name,title,path,bytes,created_at,attached_by,
    (SELECT expires_at FROM removed_space_files r WHERE r.space_id=space_files.space_id),
    space_id IN (SELECT id FROM spaces WHERE kind='chat')
        OR space_id IN (SELECT space_id FROM removed_space_files WHERE chat),
    space_id,session";

const GONE: &str = "space_id NOT IN (SELECT id FROM spaces)";

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AttachedFile> {
    let kind = match row.get(10)? {
        true => SpaceKind::Chat,
        false => SpaceKind::Workspace,
    };
    Ok(AttachedFile {
        id: row.get(0)?,
        space: AttachedTo {
            id: row.get(11)?,
            name: row.get(1)?,
            kind,
        },
        source: row.get(2)?,
        name: row.get(3)?,
        title: row.get(4)?,
        copy: KeptCopy::Here {
            path: PathBuf::from(row.get::<_, String>(5)?),
        },
        bytes: row.get::<_, i64>(6)?.try_into().unwrap_or_default(),
        created_at: row.get(7)?,
        attached_by: serde_json::from_str(&row.get::<_, String>(8)?).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Text, error.into())
        })?,
        expires_at: row.get(9)?,
        session: (row.get::<_, Option<String>>(12)?)
            .map(|session| serde_json::from_str(&session))
            .transpose()
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    12,
                    rusqlite::types::Type::Text,
                    error.into(),
                )
            })?,
    })
}

fn space(row: &rusqlite::Row<'_>) -> rusqlite::Result<Space> {
    Ok(Space {
        id: row.get(0)?,
        name: row.get(1)?,
        repository_id: row.get(2)?,
        parent_id: row.get(3)?,
        root_dir: row
            .get::<_, Option<String>>(4)?
            .map(std::path::PathBuf::from),
    })
}

/// Runs in the caller's transaction. A copy already recorded, as when the
/// same files are sent again, keeps its record and takes any new title.
/// Nothing is recorded for a space that is gone.
pub fn record(
    tx: &Connection,
    space_id: &SpaceId,
    source: FileSource,
    attacher: &Attacher,
    copies: &[Copy],
) -> Result<()> {
    let attacher = serde_json::to_string(attacher)?;
    for copy in copies {
        let name = copy
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let session = (copy.session.as_ref())
            .map(serde_json::to_string)
            .transpose()?;
        tx.execute(
            "INSERT INTO space_files
                (space_id,space,parent_id,repository_id,root_dir,source,name,title,path,bytes,
                    created_at,attached_by,session)
                SELECT spaces.id,spaces.name,
                    (SELECT parent_id FROM space_parents WHERE space_id=?1),
                    spaces.repository_id,chats.root_dir,?2,?3,?4,?5,?6,unixepoch(),?7,?8
                FROM spaces LEFT JOIN chats USING(id) WHERE spaces.id=?1
                ON CONFLICT(path) DO UPDATE SET title=COALESCE(excluded.title,title)",
            params![
                space_id,
                source,
                name,
                copy.title,
                copy.path.to_string_lossy(),
                i64::try_from(copy.bytes)?,
                attacher,
                session
            ],
        )?;
    }
    Ok(())
}

/// Returns the copies' records in their order.
pub(super) fn record_all(
    tx: &Transaction<'_>,
    space_id: &SpaceId,
    source: FileSource,
    attacher: &Attacher,
    copies: &[Copy],
) -> Result<Vec<AttachedFile>> {
    record(tx, space_id, source, attacher, copies)?;
    let records = {
        let mut statement =
            tx.prepare(&format!("SELECT {COLUMNS} FROM space_files WHERE path=?1"))?;
        copies
            .iter()
            .map(|copy| Ok(statement.query_row([copy.path.to_string_lossy()], row)?))
            .collect::<Result<Vec<_>>>()?
    };
    Ok(records)
}

/// In their order. A copy whose record is gone drops out.
pub(super) fn at(db: &Connection, paths: &[PathBuf]) -> Result<Vec<AttachedFile>> {
    let mut statement = db.prepare(&format!("SELECT {COLUMNS} FROM space_files WHERE path=?1"))?;
    let mut records = Vec::new();
    for path in paths {
        let found = statement
            .query_row([path.to_string_lossy()], row)
            .optional()?;
        records.extend(found);
    }
    Ok(records)
}

/// In their order. A file whose record is gone drops out.
pub(super) fn sent(db: &Connection, files: &[SentFile]) -> Result<Vec<AttachedFile>> {
    let mut statement = db.prepare(&format!(
        "SELECT {COLUMNS} FROM space_files WHERE id=?1 AND space_id=?2"
    ))?;
    let mut records = Vec::new();
    for file in files {
        let found = statement
            .query_row(params![file.id, file.space], row)
            .optional()?;
        records.extend(found);
    }
    Ok(records)
}

/// Oldest first.
pub(super) fn list(db: &Connection, space_id: &SpaceId) -> Result<Vec<AttachedFile>> {
    Ok(db
        .prepare(&format!(
            "SELECT {COLUMNS} FROM space_files WHERE space_id=?1 ORDER BY id"
        ))?
        .query_map([space_id], row)?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn count(db: &Connection, space_id: &SpaceId) -> Result<usize> {
    let count: i64 = db.query_row(
        "SELECT COUNT(*) FROM space_files WHERE space_id=?1",
        [space_id],
        |row| row.get(0),
    )?;
    Ok(usize::try_from(count)?)
}

/// Files whose workspace has no recorded execution, running or unresolved,
/// and that no running chat owns. Oldest first.
pub(super) fn idle(db: &Connection, home: &HomeKey) -> Result<Vec<AttachedFile>> {
    let running = crate::executions::spaces_with_executions(db)?;
    let files: Vec<AttachedFile> = db
        .prepare(&format!(
            "SELECT {COLUMNS} FROM space_files WHERE {IN_HOME}
                AND space_id NOT IN (SELECT id FROM spaces WHERE kind='chat')
                ORDER BY id"
        ))?
        .query_map(params![home.repository_id(), home.root_dir()], row)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(files
        .into_iter()
        .filter(|file| !running.contains(&file.space.id))
        .collect())
}

/// A record is in the home of the repository `?1`, or without one in the
/// `root_dir` `?2`.
const IN_HOME: &str = "repository_id IS ?1 AND (?1 IS NOT NULL OR root_dir IS ?2)";

pub fn roots(db: &Connection) -> Result<Vec<String>> {
    Ok(db
        .prepare(
            "SELECT DISTINCT root_dir FROM space_files
                WHERE repository_id IS NULL AND root_dir IS NOT NULL ORDER BY root_dir",
        )?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

pub(super) fn gone_space(db: &Connection, id: &SpaceId) -> Result<Option<Space>> {
    Ok(db
        .query_row(
            &format!(
                "SELECT space_id,space,repository_id,parent_id,root_dir FROM space_files
                    WHERE space_id=?1 AND {GONE}
                    ORDER BY id DESC LIMIT 1"
            ),
            [id],
            space,
        )
        .optional()?)
}

/// Those that attached them first come first.
pub(super) fn kept(db: &Connection) -> Result<Vec<Space>> {
    Ok(db
        .prepare(&format!(
            "SELECT space_id,space,repository_id,parent_id,root_dir FROM space_files
                WHERE {GONE} GROUP BY space_id ORDER BY MIN(id)"
        ))?
        .query_map([], space)?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn kept_ids(db: &Connection) -> Result<Vec<SpaceId>> {
    Ok(db
        .prepare(&format!(
            "SELECT DISTINCT space_id FROM space_files WHERE {GONE} ORDER BY space_id"
        ))?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Includes spaces that never had an expiry. The record stays until the
/// files are deleted, so a failed deletion still lists them as the
/// space's.
pub(super) fn expired(db: &Connection, home: &HomeKey) -> Result<Vec<SpaceId>> {
    Ok(db
        .prepare(&format!(
            "SELECT DISTINCT space_id FROM space_files WHERE {IN_HOME} AND {GONE}
                AND space_id NOT IN (SELECT space_id FROM removed_space_files
                    WHERE expires_at > unixepoch())"
        ))?
        .query_map(params![home.repository_id(), home.root_dir()], |row| {
            row.get(0)
        })?
        .collect::<rusqlite::Result<_>>()?)
}

pub(super) fn any_in(db: &Connection, directories: &[PathBuf]) -> Result<bool> {
    for directory in directories {
        let prefix = format!("{}/", directory.to_string_lossy());
        if crate::store::exists(
            db,
            "SELECT 1 FROM space_files WHERE substr(path,1,length(?1))=?1",
            [&prefix],
        )? {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn forget(tx: &Transaction<'_>, ids: &[i64]) -> Result<()> {
    for id in ids {
        tx.execute("DELETE FROM space_files WHERE id=?1", [id])?;
    }
    Ok(())
}

pub(super) fn forget_space(tx: &Transaction<'_>, space_id: &SpaceId) -> Result<()> {
    tx.execute("DELETE FROM space_files WHERE space_id=?1", [space_id])?;
    tx.execute(
        "DELETE FROM removed_space_files WHERE space_id=?1",
        [space_id],
    )?;
    Ok(())
}

/// The files keep the space's last name. Moves those it was the parent of to
/// its own parent. Runs in the transaction that deletes the space's record,
/// before the record goes.
pub fn keep_removed(
    tx: &Transaction<'_>,
    space_id: &SpaceId,
    expires_at: crate::time::UnixTime,
) -> Result<()> {
    tx.execute(
        "UPDATE space_files SET space=(SELECT name FROM spaces WHERE id=?1)
            WHERE space_id=?1",
        [space_id],
    )?;
    reparent(tx, space_id)?;
    tx.execute(
        "INSERT OR REPLACE INTO removed_space_files(space_id,chat,expires_at)
            SELECT id,kind='chat',?2 FROM spaces WHERE id=?1
                AND EXISTS(SELECT 1 FROM space_files WHERE space_id=?1)",
        params![space_id, expires_at],
    )?;
    Ok(())
}

/// As when `files rm` deleted them all.
pub(super) fn forget_expired(db: &mut Connection) -> Result<()> {
    db.execute(
        "DELETE FROM removed_space_files WHERE expires_at <= unixepoch()
            AND space_id NOT IN (SELECT space_id FROM space_files)",
        [],
    )?;
    Ok(())
}

/// Under the space's name and parent now, in the transaction that records
/// the restore.
pub fn restore(tx: &Transaction<'_>, id: &SpaceId) -> Result<()> {
    tx.execute(
        "UPDATE space_files SET space=(SELECT name FROM spaces WHERE id=?1),
            parent_id=(SELECT parent_id FROM space_parents WHERE space_id=?1)
            WHERE space_id=?1",
        [id],
    )?;
    tx.execute("DELETE FROM removed_space_files WHERE space_id=?1", [id])?;
    Ok(())
}

/// Keeps them at least until `until`, while a restore of the space runs.
pub fn claim(tx: &Transaction<'_>, space_id: &SpaceId, until: crate::time::UnixTime) -> Result<()> {
    tx.execute(
        "UPDATE removed_space_files SET expires_at=?2 WHERE space_id=?1 AND expires_at < ?2",
        params![space_id, until],
    )?;
    Ok(())
}

fn reparent(tx: &Transaction<'_>, id: &SpaceId) -> Result<()> {
    tx.execute(
        "UPDATE space_files
            SET parent_id=(SELECT parent_id FROM space_parents WHERE space_id=?1)
            WHERE parent_id=?1",
        [id],
    )?;
    Ok(())
}

pub(super) fn forget_repository(db: &Connection, repository_id: &RepositoryId) -> Result<()> {
    db.execute(
        "DELETE FROM space_files WHERE repository_id=?1",
        [repository_id],
    )?;
    Ok(())
}

/// Runs in the transaction that moves the repository's directory.
pub fn rebase(
    tx: &Transaction<'_>,
    repository_id: &RepositoryId,
    source: &Path,
    destination: &Path,
) -> Result<()> {
    let recorded: Vec<(i64, String)> = tx
        .prepare("SELECT id,path FROM space_files WHERE repository_id=?1")?
        .query_map([repository_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (id, path) in recorded {
        if let Ok(inside) = Path::new(&path).strip_prefix(source) {
            tx.execute(
                "UPDATE space_files SET path=?2 WHERE id=?1",
                params![id, destination.join(inside).to_string_lossy()],
            )?;
        }
    }
    Ok(())
}
