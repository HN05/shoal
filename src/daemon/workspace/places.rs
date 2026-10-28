//! Where executions run in terminal hosts, as their wrappers report it, and
//! the places Shoal created for workspaces, which it closes with them.
use anyhow::Result;
use rusqlite::{Connection, params};

use super::Manager;
use crate::sessions::Origin;

impl Manager {
    /// Record that `execution` runs at `origin`.
    pub(crate) async fn record_execution_origin(
        &self,
        execution: &str,
        origin: Origin,
    ) -> Result<()> {
        let execution = execution.to_owned();
        let origin = serde_json::to_string(&origin)?;
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE executions SET origin=?2 WHERE id=?1",
                    params![execution, origin],
                )?;
                Ok(())
            })
            .await
    }

    /// Record `origin`, a place Shoal created for the workspace `selector`
    /// names, as owned by it.
    pub(crate) async fn record_owned_place(
        &self,
        selector: &str,
        mut origin: Origin,
    ) -> Result<()> {
        let workspace = self.workspace(selector).await?;
        anyhow::ensure!(
            origin.place.is_some(),
            "a {} place needs its ID to be recorded",
            origin.host
        );
        origin.owned = true;
        self.store
            .run(move |db| {
                db.execute(
                    "INSERT OR REPLACE INTO owned_places(host,place,workspace_id,origin)
                     VALUES (?1,?2,?3,?4)",
                    params![
                        origin.host,
                        origin.id(),
                        workspace.id,
                        serde_json::to_string(&origin)?
                    ],
                )?;
                Ok(())
            })
            .await
    }

    /// The places Shoal created for `workspace`, or for every workspace,
    /// with the workspace each belongs to.
    pub(super) async fn owned_places(
        &self,
        workspace: Option<String>,
    ) -> Result<Vec<(String, Origin)>> {
        self.store
            .run(move |db| owned(db, workspace.as_deref()))
            .await
    }

    /// Forget `place` once it is closed.
    pub(super) async fn forget_owned_place(&self, place: Origin) -> Result<()> {
        self.store
            .run(move |db| {
                db.execute(
                    "DELETE FROM owned_places WHERE host=?1 AND place=?2",
                    params![place.host, place.id()],
                )?;
                Ok(())
            })
            .await
    }
}

fn owned(db: &Connection, workspace: Option<&str>) -> Result<Vec<(String, Origin)>> {
    let mut statement = db.prepare(
        "SELECT workspace_id,origin FROM owned_places
         WHERE ?1 IS NULL OR workspace_id=?1 ORDER BY rowid",
    )?;
    let rows = statement.query_map([workspace], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.map(|row| {
        let (workspace, origin) = row?;
        Ok((workspace, serde_json::from_str(&origin)?))
    })
    .collect()
}

#[cfg(test)]
mod tests;
