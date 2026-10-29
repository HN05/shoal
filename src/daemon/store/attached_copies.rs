//! Record the copies of files attached to messages before Shoal recorded
//! them, so `shoal files` lists and deletes them too. Each existing
//! workspace keeps them in its directory below its repository's attached
//! files, one directory per message and one per file within it.
use std::path::Path;

use anyhow::Result;
use rusqlite::Connection;

use crate::daemon::workspace_files::{FileSource, layout::Layout, record};

pub(super) fn record_existing(db: &Connection) -> Result<()> {
    let workspaces: Vec<(String, String, String)> = db
        .prepare(
            "SELECT w.id,r.id,r.workspaces_dir FROM workspaces w
                JOIN repositories r ON r.id=w.repository_id WHERE r.workspaces_dir IS NOT NULL",
        )?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (workspace_id, repository_id, workspaces_dir) in workspaces {
        let layout = Layout::new(Path::new(&workspaces_dir), &repository_id);
        let copies = layout.workspace_copies(&workspace_id);
        let bytes: Vec<u64> = copies
            .iter()
            .map(|copy| copy.metadata().map_or(0, |metadata| metadata.len()))
            .collect();
        record(db, &workspace_id, FileSource::Message, &copies, &bytes)?;
    }
    Ok(())
}
