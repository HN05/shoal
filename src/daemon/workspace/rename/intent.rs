//! Durable rename intent: the reserved name and branch a workspace is moving to.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

use super::Manager;
use crate::{git, model::Workspace};

/// The identity a workspace takes once Git has renamed its branch.
pub(super) struct RenameIntent {
    pub name: String,
    pub branch: String,
}

impl RenameIntent {
    /// Reserve the name and branch inside the caller's transaction.
    pub(super) fn record(&self, db: &Connection, workspace_id: &str) -> Result<()> {
        db.execute(
            "INSERT INTO workspace_renames(workspace_id,name,branch) VALUES (?1,?2,?3)",
            params![workspace_id, self.name, self.branch],
        )?;
        Ok(())
    }
}

impl Manager {
    pub(super) async fn rename_intent(&self, workspace_id: &str) -> Result<Option<RenameIntent>> {
        let id = workspace_id.to_owned();
        self.store
            .run(move |db| {
                Ok(db
                    .query_row(
                        "SELECT name,branch FROM workspace_renames WHERE workspace_id=?1",
                        [id],
                        |row| {
                            Ok(RenameIntent {
                                name: row.get(0)?,
                                branch: row.get(1)?,
                            })
                        },
                    )
                    .optional()?)
            })
            .await
    }

    /// Take the intended identity and move dependent bases to the new branch,
    /// releasing the reservation in the same transaction.
    pub(super) async fn commit_rename(
        &self,
        workspace: &Workspace,
        intent: RenameIntent,
    ) -> Result<()> {
        let id = workspace.id.clone();
        let repository_id = workspace.repository_id.clone();
        let old_base = git::local_ref(&workspace.branch);
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                tx.execute(
                    "UPDATE workspaces SET name=?2,branch=?3 WHERE id=?1",
                    params![id, intent.name, intent.branch],
                )?;
                tx.execute(
                    "UPDATE workspaces SET base_ref=?3 WHERE repository_id=?1 AND base_ref=?2",
                    params![repository_id, old_base, git::local_ref(&intent.branch)],
                )?;
                release(&tx, &id)?;
                tx.commit()?;
                Ok(())
            })
            .await
    }

    /// Release the reservation and keep the recorded identity.
    pub(super) async fn cancel_rename(&self, workspace_id: &str) -> Result<()> {
        let id = workspace_id.to_owned();
        self.store.run(move |db| release(db, &id)).await
    }
}

fn release(db: &Connection, workspace_id: &str) -> Result<()> {
    db.execute(
        "DELETE FROM workspace_renames WHERE workspace_id=?1",
        [workspace_id],
    )?;
    Ok(())
}
