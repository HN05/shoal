//! The checks a change makes in its transaction before it goes ahead.
use std::path::Path;

use anyhow::{Result, ensure};
use rusqlite::Connection;
use shoal_core::refusal::{Reason, Refusal};
use shoal_ids::{
    repository_ids::RepositoryId,
    space_ids::{SpaceId, SpaceNumber},
};
use shoal_protocol::space_names::SpaceName;
use shoal_protocol::state::WorkspaceState;

use crate::sql::exists;

/// Refuse a new workspace path that contains or lies inside a recorded
/// workspace's path, whether or not that directory still exists.
pub fn ensure_path_unrecorded(db: &Connection, path: &Path) -> Result<()> {
    let recorded = db
        .prepare("SELECT path FROM workspaces")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for other in recorded.iter().map(Path::new) {
        ensure!(
            !path.starts_with(other) && !other.starts_with(path),
            "workspace path overlaps a recorded workspace: {}",
            other.display()
        );
    }
    Ok(())
}

/// Fail while a workspace has an unfinished rename; lifecycle operations
/// must wait for repair.
pub fn ensure_no_pending_rename(db: &Connection, workspace_id: &SpaceId) -> Result<()> {
    ensure!(
        !exists(
            db,
            "SELECT 1 FROM workspace_renames WHERE workspace_id=?1",
            [workspace_id]
        )?,
        "workspace has an unfinished rename; run shoal branch rename again to finish it"
    );
    Ok(())
}

/// Refuse lifecycle changes while the workspace's repository directory moves.
pub fn ensure_no_pending_move(db: &Connection, workspace_id: &SpaceId) -> Result<()> {
    ensure!(
        !exists(
            db,
            "SELECT 1 FROM repository_moves JOIN spaces USING (repository_id)
                WHERE spaces.id=?1 AND spaces.kind='workspace'",
            [workspace_id]
        )?,
        "repository move is incomplete; run shoal repo move again"
    );
    Ok(())
}

/// Refuse new users of a repository whose directory is moving.
pub fn ensure_repository_not_moving(db: &Connection, repository_id: &RepositoryId) -> Result<()> {
    ensure!(
        !exists(
            db,
            "SELECT 1 FROM repository_moves WHERE repository_id=?1",
            [repository_id]
        )?,
        "repository move is incomplete; run shoal repo move again"
    );
    Ok(())
}

/// Refuse a change to the records of the space `id`, such as its links or
/// threads, while it is a workspace that is not ready; a chat takes one
/// whenever it is recorded.
pub fn require_ready_space(db: &Connection, id: &SpaceId) -> Result<()> {
    if crate::spaces::is_chat(db, id)? {
        return Ok(());
    }
    require_ready(db, id)
}

/// Fail unless the workspace is ready; resources may only change then.
pub fn require_ready(db: &Connection, workspace_id: &SpaceId) -> Result<()> {
    let (number, name, state): (SpaceNumber, SpaceName, WorkspaceState) = db.query_row(
        "SELECT s.number, s.name, w.state FROM workspaces w JOIN spaces s ON s.id=w.id
             WHERE w.id=?1",
        [workspace_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    match state {
        WorkspaceState::Ready => Ok(()),
        state => Err(not_ready(number, workspace_id, &name, state)),
    }
}

/// The refusal of a request that needs the workspace ready, naming what to
/// do about it. The daemon's first check and each recheck in a transaction
/// both refuse with it, so the two read alike.
pub fn not_ready(
    number: SpaceNumber,
    id: &SpaceId,
    name: &SpaceName,
    state: WorkspaceState,
) -> anyhow::Error {
    let remedy = match state {
        WorkspaceState::Failed => {
            format!("shoal status {number} shows why, and shoal setup {number} retries its setup")
        }
        WorkspaceState::Preparing => format!(
            "run the command again once it is ready; shoal status {number} shows how far it got"
        ),
        WorkspaceState::Removing => format!("shoal restore {id} reopens it once it is removed"),
        WorkspaceState::Ready
        | WorkspaceState::Stopping
        | WorkspaceState::Reconciling
        | WorkspaceState::Restoring
        | WorkspaceState::Updating => {
            "run the command again once that operation finishes".to_owned()
        }
    };
    let message = format!("workspace {number} ({name}) is {state}; {remedy}");
    Refusal::new(Reason::NotReady, message).into()
}

/// Whether the repository's removal has begun, which refuses new work in it.
pub fn repository_removing(db: &Connection, repository_id: &RepositoryId) -> Result<bool> {
    exists(
        db,
        "SELECT 1 FROM repository_removals WHERE repository_id=?1",
        [repository_id],
    )
}
