//! Child workspaces: workspaces split from a parent, whose agent works on them
//! or coordinates their agents. Children form a tree within one repository.
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension};

use super::Manager;
use crate::model::{Workspace, WorkspaceRef};

impl Manager {
    /// Create a workspace on a new branch named `name` in `parent`'s
    /// repository and record `parent` as its parent.
    pub async fn split_workspace(
        &self,
        parent: &str,
        name: String,
        base: Option<String>,
    ) -> Result<Workspace> {
        let parent = self.workspace(parent).await?;
        let (repo, _guard) = self.lock_repository(&parent.repository_id).await?;
        crate::git::check_branch_name(Some(&repo.path), &name).await?;
        let branch = self.available_branch(&repo, &name).await?;
        self.create_branch_workspace(
            &repo,
            name,
            branch,
            super::WorkspaceSource::New {
                base,
                parent: Some(parent.id),
            },
            None,
            None,
        )
        .await
    }

    /// Whether `id` is `ancestor` or a workspace below it.
    pub async fn within_subtree(&self, ancestor: &str, id: &str) -> Result<bool> {
        let (ancestor, id) = (ancestor.to_owned(), id.to_owned());
        self.store
            .run(move |db| Ok(subtree(db, &ancestor)?.contains(&id)))
            .await
    }

    /// `id` and every workspace below it, `id` first.
    pub async fn subtree(&self, id: &str) -> Result<Vec<String>> {
        let id = id.to_owned();
        self.store.run(move |db| subtree(db, &id)).await
    }

    /// The workspaces below `id` whose activity its agent receives: children
    /// without an agent of their own, and theirs in turn.
    pub(crate) async fn delegated_workspaces(&self, id: &str) -> Result<Vec<WorkspaceRef>> {
        let id = id.to_owned();
        self.store.run(move |db| delegated(db, &id)).await
    }
}

/// Check that `parent` is a workspace of `repository_id` before recording it.
pub(super) fn ensure_parent(db: &Connection, repository_id: &str, parent: &str) -> Result<()> {
    let repository: Option<String> = db
        .query_row(
            "SELECT repository_id FROM workspaces WHERE id=?1",
            [parent],
            |row| row.get(0),
        )
        .optional()?;
    ensure!(
        repository.as_deref() == Some(repository_id),
        "parent workspace {parent} is not a workspace of this repository"
    );
    Ok(())
}

fn subtree(db: &Connection, id: &str) -> Result<Vec<String>> {
    Ok(db
        .prepare(
            "WITH RECURSIVE tree(id, depth) AS (
                SELECT ?1, 0
                UNION SELECT workspaces.id, tree.depth + 1 FROM workspaces
                    JOIN tree ON workspaces.parent_workspace_id=tree.id
            ) SELECT id FROM tree ORDER BY depth, id",
        )?
        .query_map([id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

fn delegated(db: &Connection, id: &str) -> Result<Vec<WorkspaceRef>> {
    Ok(db
        .prepare(
            "WITH RECURSIVE delegated(id, depth) AS (
                SELECT ?1, 0
                UNION SELECT workspaces.id, delegated.depth + 1 FROM workspaces
                    JOIN delegated ON workspaces.parent_workspace_id=delegated.id
                    WHERE NOT EXISTS (SELECT 1 FROM workspace_agent_state
                        WHERE workspace_agent_state.workspace_id=workspaces.id)
            ) SELECT workspaces.id,workspaces.name,workspaces.branch FROM delegated
                JOIN workspaces USING(id) WHERE depth > 0 ORDER BY depth, name",
        )?
        .query_map([id], row)?
        .collect::<rusqlite::Result<_>>()?)
}

/// Fill in the workspace's parent and its children.
pub(super) fn load(db: &Connection, workspace: &mut Workspace) -> Result<()> {
    workspace.parent_workspace = db
        .query_row(
            "SELECT parent.id,parent.name,parent.branch FROM workspaces child
             JOIN workspaces parent ON parent.id=child.parent_workspace_id WHERE child.id=?1",
            [&workspace.id],
            row,
        )
        .optional()?;
    workspace.child_workspaces = db
        .prepare(
            "SELECT id,name,branch FROM workspaces WHERE parent_workspace_id=?1 ORDER BY name",
        )?
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

#[cfg(test)]
mod tests;
