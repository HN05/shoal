//! Caller-named claims of workspace use, independent of assignment completion.
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};

use super::{GuardMode, Manager};
use crate::{
    daemon::store,
    model::{HoldPlacer, WorkspaceHold},
    validate,
};

impl Manager {
    /// Hold the workspace as `placed_by`; repeating a held name keeps the
    /// hold as it was placed.
    pub async fn acquire_hold(
        &self,
        selector: &str,
        name: String,
        reason: Option<String>,
        placed_by: HoldPlacer,
    ) -> Result<WorkspaceHold> {
        validate::name("hold", &name)?;
        validate::reason("hold", reason.as_deref())?;
        let workspace = self.workspace(selector).await?;
        let _guard = self
            .gates
            .resource_guard(&workspace.id, GuardMode::Shared)?;
        self.verify_worktree(&workspace).await?;
        let id = workspace.id.clone();
        let hold = self.store.run(move |db| {
            let tx = db.transaction()?;
            store::require_ready(&tx, &id)?;
            tx.execute(
                "INSERT INTO workspace_holds(workspace_id,name,reason,created_at,placed_by) VALUES (?1,?2,?3,?4,?5)
                 ON CONFLICT(workspace_id,name) DO NOTHING",
                params![id, name, reason, crate::time::unix_seconds() as i64, placed_by],
            )?;
            let hold = tx.query_row(
                &format!("{COLUMNS} WHERE workspace_id=?1 AND name=?2"),
                params![id, name], row,
            )?;
            tx.commit()?;
            Ok(hold)
        }).await?;
        self.touch(&workspace.id).await;
        Ok(hold)
    }

    /// Release the hold `name` for `released_by`: an agent releases only
    /// the holds agents placed.
    pub async fn release_hold(
        &self,
        selector: &str,
        name: String,
        released_by: HoldPlacer,
    ) -> Result<()> {
        validate::name("hold", &name)?;
        let workspace = self.workspace(selector).await?;
        let id = workspace.id.clone();
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                let placed_by: Option<HoldPlacer> = tx
                    .query_row(
                        "SELECT placed_by FROM workspace_holds WHERE workspace_id=?1 AND name=?2",
                        params![id, name],
                        |row| row.get(0),
                    )
                    .optional()?;
                ensure!(
                    released_by == HoldPlacer::User || placed_by != Some(HoldPlacer::User),
                    "hold {name} is the user's; workspace processes release only the holds agents placed"
                );
                tx.execute(
                    "DELETE FROM workspace_holds WHERE workspace_id=?1 AND name=?2",
                    params![id, name],
                )?;
                tx.commit()?;
                Ok(())
            })
            .await?;
        self.touch(&workspace.id).await;
        self.signals.wake_cleanup();
        Ok(())
    }

    pub async fn holds(&self, selector: &str) -> Result<Vec<WorkspaceHold>> {
        Ok(self.workspace(selector).await?.holds)
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

const COLUMNS: &str = "SELECT workspace_id,name,reason,created_at,placed_by FROM workspace_holds";

pub(super) fn list(db: &Connection, id: &str) -> Result<Vec<WorkspaceHold>> {
    Ok(db
        .prepare(&format!("{COLUMNS} WHERE workspace_id=?1 ORDER BY name"))?
        .query_map([id], row)?
        .collect::<rusqlite::Result<_>>()?)
}

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkspaceHold> {
    Ok(WorkspaceHold {
        workspace_id: row.get(0)?,
        name: row.get(1)?,
        reason: row.get(2)?,
        created_at: row.get(3)?,
        placed_by: row.get(4)?,
    })
}

#[cfg(test)]
mod tests;
