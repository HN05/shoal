//! Base workspaces: the workspace whose branch a stacked workspace builds on.
use crate::client::repository_ids::RepositoryId;
use crate::client::space_ids::SpaceId;
use std::path::Path;

use crate::client::space_names::SpaceSelector;
use crate::refusal::{Reason, Refusal};
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};

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
    /// Record or clear the workspace whose branch this one builds on, which
    /// must change it.
    pub async fn set_base_workspace(
        &self,
        selector: &SpaceSelector,
        base: Option<String>,
    ) -> Result<Workspace> {
        let workspace = self.workspace(selector).await?;
        if base.is_some() {
            workspace.require_branch("stacking on a base workspace")?;
        }
        let repository_id = workspace.repository_id.clone();
        let base = match base {
            Some(base) => Some(
                self.store
                    .run(move |db| base_workspace(db, &repository_id, &base))
                    .await?,
            ),
            None => None,
        };
        // The PR sweep restacks under the gates of a base and the workspaces
        // stacked on it, so a change cannot land between its retarget and its
        // recorded move, nor stack a workspace on a base it is removing.
        let _gate = self
            .gates
            .lock_prs(base.iter().chain([&workspace.id]))
            .await;
        let workspace = self.workspace_by_id(&workspace.id).await?;
        let (id, repository_id) = (workspace.id.clone(), workspace.repository_id.clone());
        self.store
            .write(move |tx| change_base(tx, &repository_id, &id, base.as_ref()))
            .await?;
        self.workspace_by_id(&workspace.id).await
    }
}

/// Record the workspace `base` names as the base of workspace `id`, or clear
/// it without one, and journal the change. Fails when nothing would change.
fn change_base(
    tx: &rusqlite::Transaction<'_>,
    repository_id: &RepositoryId,
    id: &SpaceId,
    base: Option<&SpaceId>,
) -> Result<()> {
    let base = match base {
        Some(base) => Some(base_workspace(tx, repository_id, base.as_str())?),
        None => None,
    };
    if let Some(base) = &base {
        ensure!(
            !store::exists(
                tx,
                "WITH RECURSIVE chain(id) AS (
                    SELECT ?1 UNION SELECT base_workspace_id FROM workspaces
                    JOIN chain USING(id) WHERE base_workspace_id IS NOT NULL
                ) SELECT 1 FROM chain WHERE id=?2",
                [base, id],
            )?,
            "a workspace cannot build on itself or a workspace stacked on it"
        );
    }
    let changed = tx.execute(
        "UPDATE workspaces SET base_workspace_id=?2 WHERE id=?1 AND base_workspace_id IS NOT ?2",
        params![id, base],
    )?;
    match &base {
        Some(_) => ensure!(changed > 0, "the workspace already builds on that base"),
        None => ensure!(changed > 0, "the workspace has no base workspace"),
    }
    record_base_changed(tx, id, EventCause::Manual)?;
    Ok(())
}

/// Journal the workspace's current base workspace.
pub(crate) fn record_base_changed(db: &Connection, id: &SpaceId, cause: EventCause) -> Result<()> {
    db.execute(
        "INSERT INTO workspace_events(record) SELECT json_object(
            'kind',?2,'workspace_id',stacked.id,'repository_id',stacked.repository_id,
            'name',stacked.name,'path',stacked.path,'branch',stacked.branch,'cause',?3,'error',NULL,
            'base_workspace',json((SELECT json_object('id',id,'name',name,'branch',branch)
                FROM workspaces WHERE id=stacked.base_workspace_id))
        ) FROM workspaces stacked WHERE stacked.id=?1",
        params![id, EventKind::BaseChanged, cause],
    )?;
    Ok(())
}

/// A workspace of the repository named by number, ID or name, else by
/// branch.
fn base_workspace(
    db: &Connection,
    repository_id: &RepositoryId,
    selector: &str,
) -> Result<SpaceId> {
    let named = match crate::daemon::spaces::selected(db, &SpaceSelector::parse(selector))? {
        Some((crate::model::SpaceKind::Workspace, id)) => {
            let repository = db.query_row(
                "SELECT repository_id FROM workspaces WHERE id=?1",
                [&id],
                |row| row.get::<_, RepositoryId>(0),
            )?;
            Some((id, repository))
        }
        _ => None,
    };
    if let Some((id, repository)) = named {
        ensure!(
            repository == *repository_id,
            "base workspace {selector} belongs to another repository"
        );
        // A stack builds on its base's branch.
        ensure!(
            store::exists(
                db,
                "SELECT 1 FROM workspaces WHERE id=?1 AND branch IS NOT NULL",
                [&id]
            )?,
            "base workspace {selector} is at a commit and has no branch to build on"
        );
        return Ok(id);
    }
    Ok(owner(db, repository_id, selector)?
        .ok_or_else(|| Refusal::new(Reason::NotFound, format!("unknown workspace: {selector}")))?)
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
    let Ok(reference) = git::symbolic_revision(repo, base, git::run_isolated).await else {
        return Ok(None);
    };
    Ok(git::strip_local(reference.trim_end()).map(str::to_owned))
}

/// The workspace in `repository_id` that owns `branch`.
pub(super) fn owner(
    db: &Connection,
    repository_id: &RepositoryId,
    branch: &str,
) -> Result<Option<SpaceId>> {
    Ok(db
        .query_row(
            "SELECT id FROM workspaces WHERE repository_id=?1 AND branch=?2",
            params![repository_id, branch],
            |row| row.get(0),
        )
        .optional()?)
}

#[cfg(test)]
mod tests;
