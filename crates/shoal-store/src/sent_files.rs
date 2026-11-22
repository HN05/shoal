//! The files a message sent, as the column of its row that lists them
//! holds and reads them.
use anyhow::Result;
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
