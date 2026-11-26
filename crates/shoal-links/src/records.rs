//! The stored links of a space beyond its assignments.
use anyhow::{Result, bail, ensure};
use rusqlite::Connection;
use shoal_daemon_base::events::{self, EventKind};
use shoal_forge_types::items::{
    link::{ItemKind, LinkKind},
    reference::ItemRef,
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::model::Relation;

use crate::Links;

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

impl Links {
    /// Record a relation that has no lifecycle effect, in place of the item's
    /// other recorded relation; an assigned item keeps its assignment. Recording
    /// it again keeps the first record and fills in a newly known title.
    pub fn record(
        &self,
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
        self.records.ensure_accepts_links(db, id)?;
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

    /// Remove recorded items with what their links kept, returning how many
    /// were removed.
    pub fn remove(
        &self,
        db: &Connection,
        id: &SpaceId,
        relation: Relation,
        kind: Option<LinkKind>,
        url: Option<&str>,
    ) -> Result<usize> {
        self.records.ensure_accepts_links(db, id)?;
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

    /// Drop what assigned items kept while linked, their replay cursors and review
    /// marks, and journal each as unlinked.
    pub fn forget_assigned(
        &self,
        db: &Connection,
        id: &SpaceId,
        kind: ItemKind,
        urls: &[ItemRef],
    ) -> Result<()> {
        let urls: Vec<String> = urls.iter().map(ToString::to_string).collect();
        forget_activity(db, id, &urls)?;
        self.records.forget_ready_marks(db, id, &urls)?;
        for url in &urls {
            events::record_link(db, id, EventKind::Unlinked, Relation::Assigned, kind, url)?;
        }
        Ok(())
    }
}
