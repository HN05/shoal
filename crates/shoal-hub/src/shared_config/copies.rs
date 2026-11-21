//! The hub's copies of the shared files: the last edit of each that reached
//! it, under a revision above every earlier copy's.
use anyhow::Result;
use rusqlite::{OptionalExtension as _, Transaction, params};
use shoal_protocol::hub::shared_config::{Copy, Edit};

/// Keep each of `edits` that changes a file's copy as its copy now,
/// returning the copies it changed.
pub(super) fn record(tx: &Transaction<'_>, edits: Vec<Edit>) -> Result<Vec<Copy>> {
    let mut changed = Vec::new();
    for Edit { file, text } in edits {
        let current: Option<Option<String>> = tx
            .query_row(
                "SELECT text FROM hub_shared_files WHERE file=?1",
                [file.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        if current.is_some_and(|current| current == text) {
            continue;
        }
        let revision: i64 = tx.query_row(
            "SELECT COALESCE(MAX(revision), 0) + 1 FROM hub_shared_files",
            [],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT OR REPLACE INTO hub_shared_files(file, text, revision) VALUES (?1,?2,?3)",
            params![file.to_string(), text, revision],
        )?;
        changed.push(Copy {
            file,
            text,
            revision,
        });
    }
    Ok(changed)
}

/// The hub's copy of every shared file, a removed one's without text.
pub(super) fn list(tx: &Transaction<'_>) -> Result<Vec<Copy>> {
    let mut statement =
        tx.prepare("SELECT file, text, revision FROM hub_shared_files ORDER BY file")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get(1)?, row.get(2)?))
    })?;
    let mut copies = Vec::new();
    for row in rows {
        let (file, text, revision) = row?;
        copies.push(Copy {
            file: file.parse()?,
            text,
            revision,
        });
    }
    Ok(copies)
}
