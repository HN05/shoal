//! Caller-named claims of workspace use, independent of assignment completion.
use anyhow::Result;
use rusqlite::{Connection, params};

use super::{GuardMode, Manager};
use crate::{daemon::store, model::WorkspaceHold, validate};

impl Manager {
    pub async fn acquire_hold(
        &self,
        selector: &str,
        name: String,
        reason: Option<String>,
    ) -> Result<WorkspaceHold> {
        validate::name("hold", &name)?;
        validate::reason("hold", reason.as_deref())?;
        let workspace = self.workspace(selector).await?;
        let _guard = self
            .resource_guard(&workspace.id, GuardMode::Shared)
            .await?;
        self.verify_worktree(&workspace).await?;
        let id = workspace.id.clone();
        let hold = self.store.run(move |db| {
            let tx = db.transaction()?;
            store::require_ready(&tx, &id)?;
            tx.execute(
                "INSERT INTO workspace_holds(workspace_id,name,reason,created_at) VALUES (?1,?2,?3,?4)
                 ON CONFLICT(workspace_id,name) DO NOTHING",
                params![id, name, reason, crate::time::unix_seconds() as i64],
            )?;
            let hold = tx.query_row(
                "SELECT workspace_id,name,reason,created_at FROM workspace_holds WHERE workspace_id=?1 AND name=?2",
                params![id, name], row,
            )?;
            tx.commit()?;
            Ok(hold)
        }).await?;
        self.touch(&workspace.id).await;
        Ok(hold)
    }

    pub async fn release_hold(&self, selector: &str, name: String) -> Result<()> {
        validate::name("hold", &name)?;
        let workspace = self.workspace(selector).await?;
        let id = workspace.id.clone();
        self.store
            .run(move |db| {
                db.execute(
                    "DELETE FROM workspace_holds WHERE workspace_id=?1 AND name=?2",
                    params![id, name],
                )?;
                Ok(())
            })
            .await?;
        self.touch(&workspace.id).await;
        self.cleanup_notify.notify_one();
        Ok(())
    }

    pub(crate) async fn has_holds(&self, id: &str) -> Result<bool> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                store::exists(
                    db,
                    "SELECT 1 FROM workspace_holds WHERE workspace_id=?1",
                    [id],
                )
            })
            .await
    }
}

pub(super) fn list(db: &Connection, id: &str) -> Result<Vec<WorkspaceHold>> {
    Ok(db.prepare("SELECT workspace_id,name,reason,created_at FROM workspace_holds WHERE workspace_id=?1 ORDER BY name")?
        .query_map([id], row)?
        .collect::<rusqlite::Result<_>>()?)
}

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkspaceHold> {
    Ok(WorkspaceHold {
        workspace_id: row.get(0)?,
        name: row.get(1)?,
        reason: row.get(2)?,
        created_at: row.get(3)?,
    })
}

#[cfg(test)]
mod tests;
