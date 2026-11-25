//! Whether what a new workspace takes is free: its branch in its repository,
//! the name of its directory among every workspace's, and its path apart
//! from where a repository directory is moving.
use std::{collections::HashSet, ffi::OsStr, path::Path};

use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension};
use shoal_ids::{
    repository_ids::RepositoryId,
    space_ids::{SpaceId, SpaceNumber},
};
use shoal_protocol::space_names::{self, SpaceName};

use crate::refusal::{Reason, Refusal};

/// The first of `base`, `base-2`, `base-3`, ... that no workspace of any
/// repository has as its directory's name, each cut to the longest name
/// allowed.
pub fn free_directory(db: &Connection, base: &str) -> Result<String> {
    let recorded = workspace_paths(db)?;
    let taken: HashSet<&OsStr> = (recorded.iter())
        .filter_map(|path| Path::new(path).file_name())
        .collect();
    let free = (1_u64..)
        .map(|suffix| space_names::suffixed_directory(base, suffix))
        .find(|candidate| !taken.contains(OsStr::new(candidate)));
    Ok(free.expect("an unused suffix exists"))
}

/// Refuse `path`, a directory Shoal named for a new workspace, when another
/// workspace's directory took its name since it was chosen, which a start in
/// another repository may have done meanwhile.
pub fn ensure_directory_free(db: &Connection, path: &Path) -> Result<()> {
    let name = path.file_name().unwrap_or_default();
    let taken = workspace_paths(db)?
        .iter()
        .any(|recorded| Path::new(recorded).file_name() == Some(name));
    ensure!(
        !taken,
        "another workspace took the directory name {} meanwhile; run the command again",
        name.to_string_lossy()
    );
    Ok(())
}

/// Every recorded workspace's path.
fn workspace_paths(db: &Connection) -> Result<Vec<String>> {
    Ok(db
        .prepare("SELECT path FROM workspaces")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Refuse a workspace path that overlaps where a repository directory is
/// moving: a failed move deletes its destination.
pub fn ensure_path_outside_moves(db: &Connection, path: &Path) -> Result<()> {
    let destinations = db
        .prepare("SELECT destination FROM repository_moves")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for destination in destinations.iter().map(Path::new) {
        ensure!(
            !path.starts_with(destination) && !destination.starts_with(path),
            "workspace path overlaps {}, where a repository directory is moving",
            destination.display()
        );
    }
    Ok(())
}

/// Refuse `branch` of the repository when another workspace than
/// `except_id` owns it, before inserting a workspace or reserving a rename
/// in a transaction.
pub fn ensure_branch_available(
    db: &Connection,
    repository_id: &RepositoryId,
    branch: Option<&str>,
    except_id: Option<&SpaceId>,
) -> Result<()> {
    let Some(branch) = branch else {
        return Ok(());
    };
    let owner: Option<(SpaceNumber, SpaceName)> = db
        .query_row(
            "SELECT number,name FROM spaces JOIN workspaces USING(id)
                WHERE repository_id=?1 AND branch=?2 AND (?3 IS NULL OR id!=?3)",
            rusqlite::params![repository_id, branch, except_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((number, name)) = owner {
        let owner = space_names::label(number, &name);
        return Err(Refusal::new(
            Reason::NameTaken,
            format!("branch is already owned by workspace {owner}"),
        )
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
