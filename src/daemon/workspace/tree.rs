//! Child workspaces: workspaces split from a parent, whose agent works on them
//! or coordinates their agents. Children form a tree within one repository.
use anyhow::{Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension};

use super::Manager;
use crate::{
    model::{Workspace, WorkspaceRef},
    state::WorkspaceState,
};

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

    /// Queue `text` about the workspace named `name` for the agent of its
    /// nearest ancestor that has one, which coordinates its work. Returns
    /// false when no ancestor has an agent, its messages are full, or the
    /// event comes from an agent and the workspace has none of its own, since
    /// the coordinator itself or the user acted there; the user then
    /// receives the event instead.
    pub(crate) async fn tell_coordinator(&self, name: &str, text: String, source: Source) -> bool {
        let lookup = name.to_owned();
        let found = self
            .store
            .run(move |db| coordinator(db, &lookup, source))
            .await;
        let coordinator = match found {
            Ok(Some(coordinator)) => coordinator,
            Ok(None) => return false,
            Err(error) => {
                crate::daemon::log!("coordinator of {name}: {error:#}");
                return false;
            }
        };
        match self.queue_agent_message(&coordinator, text).await {
            Ok(()) => true,
            Err(error) => {
                crate::daemon::log!("message for the coordinator of {name}: {error:#}");
                false
            }
        }
    }

    /// The workspaces below `id` whose activity its agent receives: children
    /// without an agent of their own, and theirs in turn.
    pub(crate) async fn delegated_workspaces(&self, id: &str) -> Result<Vec<WorkspaceRef>> {
        let id = id.to_owned();
        self.store.run(move |db| delegated(db, &id)).await
    }
}

/// Check that `parent` is a ready workspace of `repository_id` before
/// recording it. Removal reserves the parent first and rechecks its children
/// afterwards, so a split either precedes that recheck or is refused here.
pub(super) fn ensure_parent(db: &Connection, repository_id: &str, parent: &str) -> Result<()> {
    let recorded: Option<(String, WorkspaceState)> = db
        .query_row(
            "SELECT repository_id,state FROM workspaces WHERE id=?1",
            [parent],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((repository, state)) = recorded else {
        bail!("parent workspace {parent} no longer exists");
    };
    ensure!(
        repository == repository_id,
        "parent workspace {parent} is not a workspace of this repository"
    );
    ensure!(
        state == WorkspaceState::Ready,
        "parent workspace is {state}; split from a ready workspace"
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

/// The workspaces below `id` whose activity its agent receives.
pub(crate) fn delegated(db: &Connection, id: &str) -> Result<Vec<WorkspaceRef>> {
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

/// The workspace `id` and those below it whose activity its agent receives,
/// each as the path of workspaces down to it from the topmost workspace that
/// receives `id`'s own activity: `id` itself when it has an agent, otherwise
/// the nearest ancestor that has one, or the root.
pub(crate) fn receiving_paths(db: &Connection, id: &str) -> Result<Vec<Vec<String>>> {
    let receivers = receiving_ancestors(db, id)?;
    Ok(delegation_paths(db, id)?
        .into_iter()
        .map(|path| receivers.iter().cloned().chain(path).collect())
        .collect())
}

/// The ancestors that receive `id`'s activity, topmost first: each parent
/// while the workspace below it has no agent of its own.
fn receiving_ancestors(db: &Connection, id: &str) -> Result<Vec<String>> {
    Ok(db
        .prepare(
            "WITH RECURSIVE up(id, depth) AS (
                SELECT ?1, 0
                UNION SELECT workspaces.parent_workspace_id, up.depth + 1 FROM up
                    JOIN workspaces ON workspaces.id=up.id
                    WHERE workspaces.parent_workspace_id IS NOT NULL
                        AND NOT EXISTS (SELECT 1 FROM workspace_agent_state
                            WHERE workspace_agent_state.workspace_id=up.id)
            ) SELECT id FROM up WHERE depth > 0 ORDER BY depth DESC",
        )?
        .query_map([id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// The workspace `id` and those below it whose activity its agent receives,
/// each as the path of workspaces from `id` down to it.
fn delegation_paths(db: &Connection, id: &str) -> Result<Vec<Vec<String>>> {
    db.prepare(
        "WITH RECURSIVE delegated(id, depth, path) AS (
                SELECT ?1, 0, ?1
                UNION SELECT workspaces.id, delegated.depth + 1,
                        delegated.path || ',' || workspaces.id
                    FROM workspaces
                    JOIN delegated ON workspaces.parent_workspace_id=delegated.id
                    WHERE NOT EXISTS (SELECT 1 FROM workspace_agent_state
                        WHERE workspace_agent_state.workspace_id=workspaces.id)
            ) SELECT path FROM delegated ORDER BY depth, id",
    )?
    .query_map([id], |row| row.get::<_, String>(0))?
    .map(|path| Ok(path?.split(',').map(str::to_owned).collect()))
    .collect()
}

/// What caused an event a coordinator may receive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    /// The daemon, about the workspace's agent, such as its exit.
    Daemon,
    /// A process acting in the workspace, such as `shoal notify`.
    Agent,
}

/// The nearest ancestor with an agent of the workspace named `name`, for an
/// event from `source`.
fn coordinator(db: &Connection, name: &str, source: Source) -> Result<Option<String>> {
    if source == Source::Agent
        && !crate::daemon::store::exists(
            db,
            "SELECT 1 FROM workspace_agent_state JOIN workspaces
                ON workspaces.id=workspace_agent_state.workspace_id WHERE workspaces.name=?1",
            [name],
        )?
    {
        return Ok(None);
    }
    Ok(db
        .query_row(
            "WITH RECURSIVE up(id, depth) AS (
                SELECT parent_workspace_id, 1 FROM workspaces
                    WHERE name=?1 AND parent_workspace_id IS NOT NULL
                UNION SELECT workspaces.parent_workspace_id, up.depth + 1 FROM up
                    JOIN workspaces ON workspaces.id=up.id
                    WHERE workspaces.parent_workspace_id IS NOT NULL
            ) SELECT id FROM up WHERE EXISTS (SELECT 1 FROM workspace_agent_state
                WHERE workspace_agent_state.workspace_id=up.id) ORDER BY depth LIMIT 1",
            [name],
            |row| row.get(0),
        )
        .optional()?)
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
