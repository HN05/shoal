//! The items recorded as linked to a space, as lists and its status show
//! them.
use rusqlite::{Connection, Row};
use shoal_forge_types::items::link::ItemKind;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::links::pr::Registration;

use shoal_forge_types::items::reference::ItemRef;
use shoal_protocol::model::{LinkedItem, Relation, SpaceLinks};

/// Read canonical links while keeping malformed legacy PR records out of list output.
pub fn space_links(db: &Connection, space_id: &SpaceId) -> rusqlite::Result<SpaceLinks> {
    let issues = (assigned_issues(db, space_id)?.into_iter())
        .map(|issue| LinkedItem {
            url: issue.url,
            title: issue.title,
        })
        .collect();
    let mut titles = crate::pr::titles::of_space(db, space_id)?;
    let prs = (crate::pr::watched_prs(db, space_id)?.into_iter())
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
        assignees: crate::issue::assignees_of(db, space_id)?,
    })
}

/// An issue assigned to a space, as its row records it.
pub struct AssignedIssue {
    pub url: ItemRef,
    pub title: Option<String>,
    pub error: Option<String>,
}

/// The space's assigned issues in the order they were assigned.
pub fn assigned_issues(
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
pub fn recorded_items(
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

/// Take `title`, which the forge reports for the item at `url`, as each
/// title the space records for it, so its links show the item's current
/// title.
pub fn refresh_titles(db: &Connection, id: &SpaceId, url: &str, title: &str) -> anyhow::Result<()> {
    for table in ["space_issue", "space_items"] {
        db.execute(
            &format!(
                "UPDATE {table} SET title=?3 WHERE space_id=?1 AND url=?2 AND title IS NOT ?3"
            ),
            rusqlite::params![id, url, title],
        )?;
    }
    crate::pr::titles::refresh(db, id, url, title)
}

/// The URLs of the space's assigned issues in the order they were assigned.
pub fn assigned_urls(db: &Connection, id: &SpaceId) -> rusqlite::Result<Vec<ItemRef>> {
    let issues = assigned_issues(db, id)?.into_iter();
    Ok(issues.map(|issue| issue.url).collect())
}

/// The workspace's assigned issues and PRs, of `kind` or every kind.
pub fn assigned_items(
    db: &Connection,
    id: &SpaceId,
    kind: Option<ItemKind>,
) -> anyhow::Result<Vec<ItemRef>> {
    let mut items = Vec::new();
    if kind != Some(ItemKind::Issue) {
        // An unreadable registration fails the read rather than passing
        // for watching nothing, which would let completion and cleanup go on.
        let registration: Option<Registration> = (crate::pr::stored(db, id)?)
            .map(|text| serde_json::from_str(&text))
            .transpose()?;
        items.extend_from_slice(crate::pr::watched_urls(registration.as_ref()));
    }
    if kind != Some(ItemKind::Pr) {
        items.extend(assigned_urls(db, id)?);
    }
    Ok(items)
}
