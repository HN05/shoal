//! Record the copies of files attached to messages before Shoal recorded
//! them, so `shoal files` lists and deletes them too.
use std::path::{Path, PathBuf};

use anyhow::Result;
use rusqlite::Connection;

use crate::daemon::workspace_files::{FileSource, layout::Layout, record};

pub(super) fn record_existing(db: &Connection) -> Result<()> {
    for (workspace_id, layout) in workspace_layouts(db)? {
        let copies = layout.workspace_copies(&workspace_id);
        record(
            db,
            &workspace_id,
            FileSource::Message,
            &copies,
            &sizes(&copies),
        )?;
    }
    Ok(())
}

/// Each workspace, with where its repository keeps attached files; a
/// registration older than the repository's directory keeps none.
fn workspace_layouts(db: &Connection) -> Result<Vec<(String, Layout)>> {
    Ok(db
        .prepare(
            "SELECT w.id,r.id,r.workspaces_dir FROM workspaces w
                JOIN repositories r ON r.id=w.repository_id WHERE r.workspaces_dir IS NOT NULL",
        )?
        .query_map([], |row| {
            let (workspace_id, repository_id, directory): (String, String, String) =
                (row.get(0)?, row.get(1)?, row.get(2)?);
            Ok((
                workspace_id,
                Layout::new(Path::new(&directory), &repository_id),
            ))
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// The size of each of `copies`, or none for one that cannot be read.
fn sizes(copies: &[PathBuf]) -> Vec<u64> {
    copies
        .iter()
        .map(|copy| copy.metadata().map_or(0, |metadata| metadata.len()))
        .collect()
}
