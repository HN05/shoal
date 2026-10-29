//! Where executions run in terminal hosts, as their wrappers report it, and
//! the places Shoal created for workspaces, which it closes with them.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

use super::Manager;
use crate::sessions::{Origin, PlaceOwnership};

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
        let origin = &place.origin;
        tx.execute(
            "UPDATE owned_places SET workspace_id=?1 WHERE host=?2 AND server=?3 AND place=?4",
            params![workspace, origin.host, origin.server(), origin.id()],
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
        // A record that replaces an earlier instance's does so under the
        // lock its closes hold.
        let _ownership = self.place_ownership.lock().await;
        self.store
            .run(move |db| insert_owned(db, &workspace.id, &origin))
            .await
    }

    /// The places Shoal created for `workspace`, or for every workspace,
    /// with the workspace each belongs to; places a chat took are no
    /// workspace's.
    pub(crate) async fn owned_places(
        &self,
        workspace: Option<String>,
    ) -> Result<Vec<(String, Origin)>> {
        self.store
            .run(move |db| owned(db, workspace.as_deref()))
            .await
    }

    /// Whose `place` is for the workspace `selector` names: the record's
    /// owner when the daemon records it, otherwise the workspace's when its
    /// host labels it as the workspace's session.
    pub(crate) async fn place_ownership(
        &self,
        selector: &str,
        place: &Origin,
    ) -> Result<PlaceOwnership> {
        let workspace = self.workspace(selector).await?;
        if let Some((owner, _)) = self.place_owner(place).await? {
            return Ok(if owner.as_deref() == Some(workspace.id.as_str()) {
                PlaceOwnership::Workspace
            } else {
                PlaceOwnership::Elsewhere
            });
        }
        let labelled = self
            .labels_session(place.host, place.id(), &workspace.id)
            .await?;
        Ok(if labelled {
            PlaceOwnership::Workspace
        } else {
            PlaceOwnership::Unknown
        })
    }

    /// The record of `place`, with the workspace it names its owner, `None`
    /// once a chat took the place over.
    pub(super) async fn place_owner(
        &self,
        place: &Origin,
    ) -> Result<Option<(Option<String>, Origin)>> {
        let (host, server, place) = (place.host, place.server(), place.id().to_owned());
        self.store
            .run(move |db| {
                db.query_row(
                    "SELECT workspace_id,origin FROM owned_places
                     WHERE host=?1 AND server=?2 AND place=?3",
                    params![host, server, place],
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
        let (host, server, place) = (place.host, place.server(), place.id().to_owned());
        let _ownership = self.place_ownership.lock().await;
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE owned_places SET workspace_id=NULL
                     WHERE host=?1 AND server=?2 AND place=?3",
                    params![host, server, place],
                )?;
                Ok(())
            })
            .await
    }

    /// Forget `place` once it is closed.
    pub(super) async fn forget_owned_place(&self, place: Origin) -> Result<()> {
        self.store.run(move |db| forget_owned(db, &place)).await
    }
}

/// Forget the record of `place`, the instance it names: a later instance
/// under its ID keeps its own record.
fn forget_owned(db: &Connection, place: &Origin) -> Result<()> {
    db.execute(
        "DELETE FROM owned_places WHERE host=?1 AND server=?2 AND place=?3
         AND json_extract(origin,'$.instance') IS ?4",
        params![place.host, place.server(), place.id(), place.instance],
    )?;
    Ok(())
}

/// Record `origin` as owned by `workspace` unless that place is recorded
/// already. A record of an earlier instance under the same ID, which a host
/// that reuses IDs left, gives way to it.
fn insert_owned(db: &Connection, workspace: &str, origin: &Origin) -> Result<()> {
    db.execute(
        "INSERT INTO owned_places(host,server,place,workspace_id,origin)
         VALUES (?1,?2,?3,?4,?5) ON CONFLICT(host,server,place) DO UPDATE
         SET workspace_id=excluded.workspace_id, origin=excluded.origin
         WHERE json_extract(excluded.origin,'$.instance') IS NOT NULL
           AND json_extract(excluded.origin,'$.instance')
               IS NOT json_extract(owned_places.origin,'$.instance')",
        params![
            origin.host,
            origin.server(),
            origin.id(),
            workspace,
            serde_json::to_string(origin)?
        ],
    )?;
    Ok(())
}

pub(super) fn owned(db: &Connection, workspace: Option<&str>) -> Result<Vec<(String, Origin)>> {
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
