//! Where executions run in terminal hosts, as their wrappers report it, and
//! the places Shoal created for workspaces, which it closes with them.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

use super::Manager;
use crate::sessions::{HostName, Origin};

/// Where an execution runs in a terminal host, as its wrapper reports it.
pub(crate) struct ExecutionPlace {
    pub origin: Origin,
    /// It runs an agent, which takes over a place Shoal created for another
    /// workspace, as a relabelled zmx session does, so the earlier
    /// workspace's end leaves it open. A command such as a nested `shoal
    /// run` takes nothing.
    pub agent: bool,
}

/// Record `place` for `execution` of `workspace` in its registration, so
/// an agent that cannot take its place over never starts.
pub(super) fn record_execution_place(
    tx: &rusqlite::Transaction<'_>,
    execution: &str,
    workspace: &str,
    place: &ExecutionPlace,
) -> Result<()> {
    tx.execute(
        "UPDATE executions SET origin=?2 WHERE id=?1",
        params![execution, serde_json::to_string(&place.origin)?],
    )?;
    if place.agent {
        tx.execute(
            "UPDATE owned_places SET workspace_id=?1 WHERE host=?2 AND place=?3",
            params![workspace, place.origin.host, place.origin.id()],
        )?;
    }
    Ok(())
}

impl Manager {
    /// Record `origin`, a place Shoal created for the workspace `selector`
    /// names, as owned by it. A place already recorded keeps its owner: only
    /// an agent's takeover or a chat moves it.
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
            .run(move |db| insert_owned(db, &workspace.id, &origin))
            .await
    }

    /// The places Shoal created for `workspace`, or for every workspace,
    /// with the workspace each belongs to; places a chat took are no
    /// workspace's.
    pub(super) async fn owned_places(
        &self,
        workspace: Option<String>,
    ) -> Result<Vec<(String, Origin)>> {
        self.store
            .run(move |db| owned(db, workspace.as_deref()))
            .await
    }

    /// The record of `place` of `host`, with the workspace it names its
    /// owner, `None` once a chat took the place over.
    pub(super) async fn place_owner(
        &self,
        host: HostName,
        place: &str,
    ) -> Result<Option<(Option<String>, Origin)>> {
        let place = place.to_owned();
        self.store
            .run(move |db| {
                db.query_row(
                    "SELECT workspace_id,origin FROM owned_places WHERE host=?1 AND place=?2",
                    params![host, place],
                    |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()?
                .map(|(workspace, origin)| Ok((workspace, serde_json::from_str(&origin)?)))
                .transpose()
            })
            .await
    }

    /// Take `place` from the workspace it was created for, as a chat
    /// starting there does: no workspace's end closes it then.
    pub(crate) async fn release_place(&self, place: &Origin) -> Result<()> {
        let (host, place) = (place.host, place.id().to_owned());
        let _ownership = self.place_ownership.lock().await;
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE owned_places SET workspace_id=NULL WHERE host=?1 AND place=?2",
                    params![host, place],
                )?;
                Ok(())
            })
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

/// Record `origin` as owned by `workspace` unless it is recorded already.
fn insert_owned(db: &Connection, workspace: &str, origin: &Origin) -> Result<()> {
    db.execute(
        "INSERT INTO owned_places(host,place,workspace_id,origin)
         VALUES (?1,?2,?3,?4) ON CONFLICT(host,place) DO NOTHING",
        params![
            origin.host,
            origin.id(),
            workspace,
            serde_json::to_string(origin)?
        ],
    )?;
    Ok(())
}

fn owned(db: &Connection, workspace: Option<&str>) -> Result<Vec<(String, Origin)>> {
    let mut statement = db.prepare(
        "SELECT workspace_id,origin FROM owned_places
         WHERE workspace_id IS NOT NULL AND (?1 IS NULL OR workspace_id=?1)
         ORDER BY rowid",
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
