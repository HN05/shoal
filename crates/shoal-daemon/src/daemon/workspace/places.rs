//! Where executions run in terminal hosts, as their wrappers report it, and
//! the places Shoal created for spaces, workspaces and chats alike, which it
//! closes with them.
use crate::client::execution_ids::ExecutionId;
use crate::client::space_ids::SpaceId;
use crate::sessions::OriginExt;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

use super::Manager;
use crate::client::space_names::SpaceSelector;
use crate::{
    model::Chat,
    sessions::{Origin, PlaceOwnership},
};

/// Where an execution runs in a terminal host, as its wrapper reports it.
pub(crate) struct ExecutionPlace {
    pub origin: Origin,
    /// It runs an agent, which takes over a place Shoal created for another
    /// space, as a relabelled zmx session does, so the earlier space's end
    /// leaves it open. A command such as a nested `shoal run` takes nothing.
    pub agent: bool,
}

/// Record `place` for `execution` of `workspace` in its registration, so
/// an agent that cannot take its place over never starts.
pub(super) fn record_execution_place(
    tx: &rusqlite::Transaction<'_>,
    execution: &ExecutionId,
    workspace: &SpaceId,
    place: &ExecutionPlace,
) -> Result<()> {
    tx.execute(
        "UPDATE executions SET origin=?2 WHERE id=?1",
        params![execution, serde_json::to_string(&place.origin)?],
    )?;
    if place.agent {
        take_over(tx, workspace, &place.origin)?;
    }
    Ok(())
}

/// Record `place`, where the chat `chat` runs, as the chat's: a place
/// Shoal created for another space passes to it, as to an agent that
/// starts there, and one Shoal opened for the chat is recorded, so the
/// chat's removal closes either.
fn record_chat_place(db: &Connection, chat: &SpaceId, place: &Origin) -> Result<()> {
    if place.owned && place.place.is_some() {
        insert_owned(db, chat, place)?;
    }
    take_over(db, chat, place)
}

/// Whether a running chat or an agent of a space other than `space` is
/// recorded at `place`, which it took over even before a record or label
/// of the place says so; a nested command takes nothing.
pub(super) fn runs_elsewhere(db: &Connection, place: &Origin, space: &SpaceId) -> Result<bool> {
    let mut statement = db.prepare(
        "SELECT origin FROM chats WHERE restore IS NULL AND id IS NOT ?1
           AND json_extract(origin,'$.host')=?2 AND json_extract(origin,'$.place')=?3
         UNION ALL
         SELECT origin FROM executions WHERE workspace_id IS NOT ?1 AND agent IS NOT NULL
           AND json_extract(origin,'$.host')=?2 AND json_extract(origin,'$.place')=?3",
    )?;
    let origins = statement.query_map(params![space, place.host, place.id()], |row| {
        row.get::<_, String>(0)
    })?;
    for origin in origins {
        if serde_json::from_str::<Origin>(&origin?)?.server() == place.server() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Make `space` the owner of `place` when the daemon records it.
fn take_over(db: &Connection, space: &SpaceId, place: &Origin) -> Result<()> {
    db.execute(
        "UPDATE owned_places SET space_id=?1 WHERE host=?2 AND server=?3 AND place=?4",
        params![space, place.host, place.server(), place.id()],
    )?;
    Ok(())
}

impl Manager {
    /// Record `origin`, a place Shoal created for the workspace `selector`
    /// names, as owned by it. A place already recorded keeps its owner: only
    /// an agent's takeover or a chat moves it.
    pub(crate) async fn record_owned_place(
        &self,
        selector: &SpaceSelector,
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

    /// Record the place the started `chat` runs in as the chat's, under the
    /// lock place closes hold. Only a chat that started takes it, so a
    /// start that fails leaves the place as it found it.
    pub(crate) async fn take_chat_place(&self, chat: &Chat) -> Result<()> {
        let Some(place) = chat.origin.clone() else {
            return Ok(());
        };
        let id = chat.id.clone();
        let _ownership = self.place_ownership.lock().await;
        (self.store)
            .run(move |db| record_chat_place(db, &id, &place))
            .await
    }

    /// Whether a running chat or an agent of a space other than `space` is
    /// recorded at `place`, which no removal of `space` closes.
    pub(super) async fn place_runs_elsewhere(
        &self,
        place: &Origin,
        space: &SpaceId,
    ) -> Result<bool> {
        let (place, space) = (place.clone(), space.clone());
        self.store
            .run(move |db| runs_elsewhere(db, &place, &space))
            .await
    }

    /// The places Shoal created for `space`, or for every space, with the
    /// space each belongs to.
    pub(crate) async fn owned_places(
        &self,
        space: Option<SpaceId>,
    ) -> Result<Vec<(SpaceId, Origin)>> {
        self.store.run(move |db| owned(db, space.as_ref())).await
    }

    /// Whose `place` is for the workspace `selector` names: the record's
    /// owner when the daemon records it, otherwise the workspace's when its
    /// host labels it as the workspace's session.
    pub(crate) async fn place_ownership(
        &self,
        selector: &SpaceSelector,
        place: &Origin,
    ) -> Result<PlaceOwnership> {
        let workspace = self.workspace(selector).await?;
        if let Some((owner, _)) = self.place_owner(place).await? {
            return Ok(if owner == workspace.id {
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

    /// The record of `place`, with the space it names its owner.
    pub(super) async fn place_owner(&self, place: &Origin) -> Result<Option<(SpaceId, Origin)>> {
        let (host, server, place) = (place.host, place.server(), place.id().to_owned());
        self.store
            .run(move |db| {
                db.query_row(
                    "SELECT space_id,origin FROM owned_places
                     WHERE host=?1 AND server=?2 AND place=?3",
                    params![host, server, place],
                    |row| Ok((row.get::<_, SpaceId>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()?
                .map(|(space, origin)| Ok((space, serde_json::from_str(&origin)?)))
                .transpose()
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

/// Record `origin` as owned by `space` unless that place is recorded
/// already. A record of an earlier instance under the same ID, which a host
/// that reuses IDs left, gives way to it.
fn insert_owned(db: &Connection, space: &SpaceId, origin: &Origin) -> Result<()> {
    db.execute(
        "INSERT INTO owned_places(host,server,place,space_id,origin)
         VALUES (?1,?2,?3,?4,?5) ON CONFLICT(host,server,place) DO UPDATE
         SET space_id=excluded.space_id, origin=excluded.origin
         WHERE json_extract(excluded.origin,'$.instance') IS NOT NULL
           AND json_extract(excluded.origin,'$.instance')
               IS NOT json_extract(owned_places.origin,'$.instance')",
        params![
            origin.host,
            origin.server(),
            origin.id(),
            space,
            serde_json::to_string(origin)?
        ],
    )?;
    Ok(())
}

pub(super) fn owned(db: &Connection, space: Option<&SpaceId>) -> Result<Vec<(SpaceId, Origin)>> {
    let mut statement = db.prepare(
        "SELECT space_id,origin FROM owned_places
         WHERE ?1 IS NULL OR space_id=?1 ORDER BY rowid",
    )?;
    let rows = statement.query_map([space], |row| {
        Ok((row.get::<_, SpaceId>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.map(|row| {
        let (space, origin) = row?;
        Ok((space, serde_json::from_str(&origin)?))
    })
    .collect()
}

#[cfg(test)]
mod tests;
