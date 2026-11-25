//! What Shoal records inside a registered repository: its spaces, the
//! executions in its workspaces, and the checkouts recorded within one of
//! its directories.
use std::path::{Path, PathBuf};

use anyhow::Result;
use rusqlite::Connection;
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::model::Execution;
use shoal_store as store;

const REPOSITORY_SPACES_QUERY: &str = "SELECT 1 FROM spaces WHERE repository_id=?1";

pub fn repository_has_spaces(db: &Connection, repository_id: &RepositoryId) -> Result<bool> {
    store::exists(db, REPOSITORY_SPACES_QUERY, [repository_id])
}

/// The IDs of the repository's spaces, workspaces and chats, by name.
pub fn repository_spaces(db: &Connection, repository_id: &RepositoryId) -> Result<Vec<SpaceId>> {
    Ok(db
        .prepare("SELECT id FROM spaces WHERE repository_id=?1 ORDER BY name, id")?
        .query_map([repository_id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Whether any workspace of the repository has a tracked execution.
pub fn repository_has_executions(db: &Connection, repository_id: &RepositoryId) -> Result<bool> {
    store::exists(
        db,
        "SELECT 1 FROM executions e JOIN spaces s ON s.id=e.space_id
            WHERE s.repository_id=?1 AND s.kind='workspace'",
        [repository_id],
    )
}

/// The executions running in the workspaces of repository `repository_id`.
pub fn repository_executions(
    db: &Connection,
    repository_id: &RepositoryId,
) -> Result<Vec<Execution>> {
    Ok(db
        .prepare(&format!(
            "SELECT {} FROM executions
                WHERE space_id IN (SELECT id FROM spaces
                    WHERE repository_id=?1 AND kind='workspace')",
            store::EXECUTION_COLUMNS
        ))?
        .query_map([repository_id], store::execution)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// A recorded checkout or workspace that lies in `directory`.
pub fn checkout_within(db: &Connection, directory: &Path) -> Result<Option<PathBuf>> {
    let recorded = db
        .prepare("SELECT path FROM repositories UNION ALL SELECT path FROM workspaces")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(recorded
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.starts_with(directory)))
}

#[cfg(test)]
mod tests;
