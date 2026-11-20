//! What a space records of the items it links beyond its assignments: the
//! relations it records them under, whether it takes link changes now, and
//! what its links kept, which goes with them.
use anyhow::{Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension};
use shoal_daemon_base::events::{self, EventKind};
use shoal_forge_types::items::link::LinkKind;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{model::Relation, state::WorkspaceState};

/// Record a relation that has no lifecycle effect, in place of the item's
/// other recorded relation; an assigned item keeps its assignment. Recording
/// it again keeps the first record and fills in a newly known title.
pub fn record(
    db: &Connection,
    id: &SpaceId,
    relation: Relation,
    kind: impl Into<LinkKind>,
    url: &str,
    title: Option<&str>,
) -> Result<()> {
    let kind = kind.into();
    if relation == Relation::Assigned {
        bail!("assigned items are linked through their issue or PR watch");
    }
    ensure_accepts_links(db, id)?;
    ensure_unassigned(db, id, kind, url)?;
    drop_recorded(db, id, url, Some(relation))?;
    let added = db.execute(
        "INSERT INTO space_items(space_id,relation,kind,url,title)
         VALUES (?1,?2,?3,?4,?5) ON CONFLICT DO NOTHING",
        rusqlite::params![id, relation, kind, url, title],
    )?;
    if added == 0 {
        db.execute(
            "UPDATE space_items SET title=COALESCE(?4,title)
             WHERE space_id=?1 AND relation=?2 AND url=?3",
            rusqlite::params![id, relation, url, title],
        )?;
    } else {
        events::record_link(db, id, EventKind::Linked, relation, kind, url)?;
    }
    Ok(())
}

/// Refuse to record another relation of an item the space assigns.
fn ensure_unassigned(db: &Connection, id: &SpaceId, kind: LinkKind, url: &str) -> Result<()> {
    let Some(kind) = kind.item() else {
        return Ok(());
    };
    let assigned = crate::linked::assigned_items(db, id, Some(kind))?;
    ensure!(
        !assigned.iter().any(|item| item.url() == url),
        "{url} is linked as {}",
        Relation::Assigned
    );
    Ok(())
}

/// Drop the recorded relations of `url` but `kept`, journaling each as
/// unlinked. What they kept stays, so the relation that replaces them
/// carries on from it.
pub fn drop_recorded(
    db: &Connection,
    id: &SpaceId,
    url: &str,
    kept: Option<Relation>,
) -> Result<()> {
    let dropped = db
        .prepare(
            "DELETE FROM space_items WHERE space_id=?1 AND url=?2 AND relation IS NOT ?3
             RETURNING relation,kind",
        )?
        .query_map(rusqlite::params![id, url, kept], |row| {
            Ok((row.get::<_, Relation>(0)?, row.get::<_, LinkKind>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (relation, kind) in dropped {
        events::record_link(db, id, EventKind::Unlinked, relation, kind, url)?;
    }
    Ok(())
}

/// Remove recorded items with what their links kept, returning how many
/// were removed.
pub fn remove(
    db: &Connection,
    id: &SpaceId,
    relation: Relation,
    kind: Option<LinkKind>,
    url: Option<&str>,
) -> Result<usize> {
    ensure_accepts_links(db, id)?;
    let removed = db
        .prepare(
            "DELETE FROM space_items WHERE space_id=?1 AND relation=?2
             AND (?3 IS NULL OR kind=?3) AND (?4 IS NULL OR url=?4) RETURNING kind,url",
        )?
        .query_map(rusqlite::params![id, relation, kind, url], |row| {
            Ok((row.get::<_, LinkKind>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (kind, url) in &removed {
        events::record_link(db, id, EventKind::Unlinked, relation, *kind, url)?;
    }
    let urls: Vec<String> = removed.into_iter().map(|(_, url)| url).collect();
    forget_activity(db, id, &urls)?;
    Ok(urls.len())
}

/// Drop the replay cursors of items that are no longer linked.
pub fn forget_activity(db: &Connection, id: &SpaceId, urls: &[String]) -> Result<()> {
    let urls = serde_json::to_string(urls)?;
    for table in ["pr_activity", "pending_updates", "space_item_activity"] {
        db.execute(
            &format!(
                "DELETE FROM {table} WHERE space_id=?1
                 AND url IN (SELECT value FROM json_each(?2))"
            ),
            rusqlite::params![id, urls],
        )?;
    }
    Ok(())
}

/// Refuse changes while the workspace is removed or otherwise in transition,
/// so a removal record taken during removal holds its final links; a chat
/// takes them whenever it runs.
pub fn ensure_accepts_links(db: &Connection, id: &SpaceId) -> Result<()> {
    if is_chat(db, id)? {
        return Ok(());
    }
    let state = state(db, id)?;
    ensure!(
        state.accepts_links(),
        "workspace cannot change links while {state}"
    );
    Ok(())
}

/// Whether the workspace still exists and its state takes link changes now,
/// or the chat still runs.
pub fn accepts_links(db: &Connection, id: &SpaceId) -> Result<bool> {
    if is_chat(db, id)? {
        return Ok(true);
    }
    let state: Option<WorkspaceState> = db
        .query_row("SELECT state FROM workspaces WHERE id=?1", [id], |row| {
            row.get(0)
        })
        .optional()?;
    Ok(state.is_some_and(WorkspaceState::accepts_links))
}

fn state(db: &Connection, id: &SpaceId) -> Result<WorkspaceState> {
    Ok(
        db.query_row("SELECT state FROM workspaces WHERE id=?1", [id], |row| {
            row.get(0)
        })?,
    )
}

/// Whether `id` names a recorded chat, whose links change whenever it runs.
pub fn is_chat(db: &Connection, id: &SpaceId) -> Result<bool> {
    Ok(db
        .query_row(
            "SELECT 1 FROM spaces WHERE id=?1 AND kind='chat'",
            [id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}
