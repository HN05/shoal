//! Base workspaces: the workspace whose branch a stacked workspace builds on.
use std::path::Path;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};

use crate::{
    git,
    model::{Workspace, WorkspaceRef},
};

/// Fill in the workspace's base and the workspaces stacked on it.
pub(super) fn load(db: &Connection, workspace: &mut Workspace) -> Result<()> {
    workspace.base_workspace = db
        .query_row(
            "SELECT base.id,base.name,base.branch FROM workspaces stacked
             JOIN workspaces base ON base.id=stacked.base_workspace_id WHERE stacked.id=?1",
            [&workspace.id],
            row,
        )
        .optional()?;
    workspace.stacked_workspaces = db
        .prepare("SELECT id,name,branch FROM workspaces WHERE base_workspace_id=?1 ORDER BY name")?
        .query_map([&workspace.id], row)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(())
}

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkspaceRef> {
    Ok(WorkspaceRef {
        id: row.get(0)?,
        name: row.get(1)?,
        branch: row.get(2)?,
    })
}

/// The branch an explicit base names, locally or on a remote, which another
/// workspace may own. Unresolvable bases infer nothing; creation reports them.
pub(super) async fn base_branch(repo: &Path, base: &str) -> Result<Option<String>> {
    if let Some((_, branch)) = git::remote_branch(repo, base).await? {
        return Ok(Some(branch));
    }
    let Ok(reference) = git::run_isolated(repo, &["rev-parse", "--symbolic-full-name", base]).await
    else {
        return Ok(None);
    };
    Ok(git::strip_local(reference.trim_end()).map(str::to_owned))
}

/// The workspace in `repository_id` that owns `branch`.
pub(super) fn owner(db: &Connection, repository_id: &str, branch: &str) -> Result<Option<String>> {
    Ok(db
        .query_row(
            "SELECT id FROM workspaces WHERE repository_id=?1 AND branch=?2",
            [repository_id, branch],
            |row| row.get(0),
        )
        .optional()?)
}

#[cfg(test)]
mod tests;
