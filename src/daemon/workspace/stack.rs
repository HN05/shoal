//! Base workspaces: the workspace whose branch a stacked workspace builds on.
use std::path::Path;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::Manager;
use crate::{
    daemon::{
        events::{EventCause, EventKind},
        store,
    },
    git,
    model::{Workspace, WorkspaceRef},
};

impl Manager {
    /// Record or clear the workspace whose branch this one builds on.
    pub async fn set_base_workspace(
        &self,
        selector: &str,
        base: Option<String>,
    ) -> Result<Workspace> {
        let workspace = self.workspace(selector).await?;
        let (id, repository_id) = (workspace.id.clone(), workspace.repository_id);
        self.store
            .run(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let base = match &base {
                    Some(base) => Some(base_workspace(&tx, &repository_id, base)?),
                    None => None,
                };
                if let Some(base) = &base {
                    ensure!(
                        !store::exists(
                            &tx,
                            "WITH RECURSIVE chain(id) AS (
                                SELECT ?1 UNION SELECT base_workspace_id FROM workspaces
                                JOIN chain USING(id) WHERE base_workspace_id IS NOT NULL
                            ) SELECT 1 FROM chain WHERE id=?2",
                            [base, &id],
                        )?,
                        "a workspace cannot build on itself or a workspace stacked on it"
                    );
                }
                let changed = tx.execute(
                    "UPDATE workspaces SET base_workspace_id=?2 WHERE id=?1 AND base_workspace_id IS NOT ?2",
                    params![id, base],
                )?;
                if changed > 0 {
                    tx.execute(
                        "INSERT INTO workspace_events(record) SELECT json_object(
                            'kind',?2,'workspace_id',id,'repository_id',repository_id,
                            'name',name,'path',path,'branch',branch,'cause',?3,'error',NULL,
                            'base_workspace',json((SELECT json_object('id',id,'name',name,'branch',branch)
                                FROM workspaces WHERE id=?4))
                        ) FROM workspaces WHERE id=?1",
                        params![id, EventKind::BaseChanged, EventCause::Manual, base],
                    )?;
                }
                tx.commit()?;
                Ok(())
            })
            .await?;
        self.workspace(&workspace.id).await
    }
}

/// A workspace of the repository named by ID, name or branch.
fn base_workspace(db: &Connection, repository_id: &str, selector: &str) -> Result<String> {
    let named = db
        .query_row(
            "SELECT id,repository_id FROM workspaces WHERE id=?1 OR name=?1",
            [selector],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    if let Some((id, repository)) = named {
        ensure!(
            repository == repository_id,
            "base workspace {selector} belongs to another repository"
        );
        return Ok(id);
    }
    owner(db, repository_id, selector)?.with_context(|| format!("unknown workspace: {selector}"))
}

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
