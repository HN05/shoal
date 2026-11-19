//! The items recorded as linked to a space, as lists and its status show
//! them.
use rusqlite::{Connection, Row};
use shoal_ids::space_ids::SpaceId;

use crate::{
    forge::items::reference::ItemRef,
    model::{LinkedItem, Relation, SpaceLinks},
};

/// Read canonical links while keeping malformed legacy PR records out of list output.
pub(crate) fn space_links(db: &Connection, space_id: &SpaceId) -> rusqlite::Result<SpaceLinks> {
    let issues = (assigned_issues(db, space_id)?.into_iter())
        .map(|issue| LinkedItem {
            url: issue.url,
            title: issue.title,
        })
        .collect();
    let mut titles = super::pr::titles::of_space(db, space_id)?;
    let prs = (super::pr::watched_prs(db, space_id)?.into_iter())
        .map(|url| LinkedItem {
            title: titles.remove(url.url()),
            url,
        })
        .collect();
    Ok(SpaceLinks {
        issues,
        prs,
        opened: recorded_items(db, space_id, Relation::Opened)?,
        related: recorded_items(db, space_id, Relation::Related)?,
        assignees: super::issue::assignees_of(db, space_id)?,
    })
}

/// An issue assigned to a space, as its row records it.
pub(crate) struct AssignedIssue {
    pub(crate) url: ItemRef,
    pub(crate) title: Option<String>,
    pub(crate) error: Option<String>,
}

/// The space's assigned issues in the order they were assigned.
pub(crate) fn assigned_issues(
    db: &Connection,
    space_id: &SpaceId,
) -> rusqlite::Result<Vec<AssignedIssue>> {
    db.prepare("SELECT url,title,error FROM space_issue WHERE space_id=?1 ORDER BY rowid")?
        .query_map([space_id], |row| {
            Ok(AssignedIssue {
                url: row.get(0)?,
                title: row.get(1)?,
                error: row.get(2)?,
            })
        })?
        .collect()
}

/// The workspace's items with a recorded relation, oldest first.
pub(crate) fn recorded_items(
    db: &Connection,
    workspace_id: &SpaceId,
    relation: Relation,
) -> rusqlite::Result<Vec<LinkedItem>> {
    db.prepare(
        "SELECT kind,url,title FROM space_items WHERE space_id=?1 AND relation=?2
         ORDER BY created_at,rowid",
    )?
    .query_map(rusqlite::params![workspace_id, relation], |row| {
        Ok(LinkedItem {
            url: linked_url(row)?,
            title: row.get(2)?,
        })
    })?
    .collect()
}

/// The URL of a linked item's row, read as the kind the row records.
fn linked_url(row: &Row<'_>) -> rusqlite::Result<ItemRef> {
    let url: String = row.get(1)?;
    ItemRef::parse_kind(row.get(0)?, &url).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, error.into())
    })
}
