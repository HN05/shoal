//! The files a message sent, as the column of its row that lists them
//! holds and reads them.
use std::path::PathBuf;

use anyhow::Result;
use rusqlite::{OptionalExtension as _, Transaction, params};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::files::SentFile;

/// SQL for a `files` column with the files whose record IDs the JSON array
/// in `$column` lists, in its order, as a JSON array of [`SentFile`]s; a file
/// whose record is gone, as after `shoal files rm`, drops out.
#[macro_export]
macro_rules! sent_files {
    ($column:literal) => {
        concat!(
            "(SELECT json_group_array(json(file)) FROM (
                SELECT json_object('id',f.id,'space',f.space_id,'name',f.name,'title',f.title,'path',f.path) AS file
                FROM json_each(",
            $column,
            ") j JOIN space_files f ON f.id=j.value ORDER BY j.key)) AS files"
        )
    };
}
pub use crate::sent_files;

/// The record IDs of `files`, as a column [`sent_files`] reads stores them.
pub fn sent_ids(files: &[SentFile]) -> Result<String> {
    let ids: Vec<i64> = files.iter().map(|file| file.id).collect();
    Ok(serde_json::to_string(&ids)?)
}

/// The path of the copy this machine keeps of the file `id` attached to
/// `space`, while its record lasts.
pub fn kept_path(tx: &Transaction<'_>, id: i64, space: &SpaceId) -> Result<Option<PathBuf>> {
    let path: Option<String> = tx
        .query_row(
            "SELECT path FROM space_files WHERE id=?1 AND space_id=?2",
            params![id, space],
            |row| row.get(0),
        )
        .optional()?;
    Ok(path.map(PathBuf::from))
}
