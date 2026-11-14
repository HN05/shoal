//! The labels ranking reads of a space's issues and PRs, as the background
//! poll, or else a watch, last read them.
use anyhow::Result;
use rusqlite::{Connection, params};

use crate::client::{repository_ids::RepositoryId, space_ids::SpaceId};
use crate::{
    daemon::{links::pr::watched_prs, store},
    forge::{items::reference::ItemRef, updates},
};

/// Work of a repository with the labels that rank it.
pub(super) struct Labelled<Id> {
    pub id: Id,
    pub repository: RepositoryId,
    pub labels: Vec<String>,
}

/// The labels of the issues assigned to the space `id`.
pub(super) fn of_issues(db: &Connection, id: &SpaceId) -> Result<Vec<String>> {
    let issues = store::assigned_issues(db, id)?.into_iter();
    of_items(db, id, issues.map(|issue| issue.url))
}

/// The labels of the issues and PRs assigned to the space `id`.
pub(super) fn of_issues_and_prs(db: &Connection, id: &SpaceId) -> Result<Vec<String>> {
    let mut labels = of_issues(db, id)?;
    labels.extend(of_items(db, id, watched_prs(db, id)?)?);
    Ok(labels)
}

/// The labels of the space `id`'s `items`.
fn of_items(
    db: &Connection,
    id: &SpaceId,
    items: impl IntoIterator<Item = ItemRef>,
) -> Result<Vec<String>> {
    let mut labels = Vec::new();
    for url in items {
        labels.extend(of_item(db, id, &url)?);
    }
    Ok(labels)
}

/// The labels of the space `id`'s item `url`; none before they are read
/// or when its record does not read, rather than failing the rank.
fn of_item(db: &Connection, id: &SpaceId, url: &ItemRef) -> Result<Vec<String>> {
    let record: Option<String> = db.query_row(
        "SELECT COALESCE(
            (SELECT record FROM space_item_activity WHERE space_id=?1 AND url=?2),
            (SELECT record FROM pr_activity WHERE space_id=?1 AND url=?2))",
        params![id, url],
        |row| row.get(0),
    )?;
    Ok((record.as_deref())
        .and_then(|record| updates::saved_labels(record).ok())
        .unwrap_or_default())
}
