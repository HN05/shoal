//! The items a space's watches, background polls and views follow, as its
//! links and watch cursors record them.
use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use shoal_forge::updates::Snapshot;
use shoal_forge_types::items::{
    link::{ItemKind, LinkKind},
    reference::ItemRef,
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::model::Relation;

use super::cursor::cursor_record;

/// The workspace's items of a kind, or of every kind, that one use of its
/// links covers.
pub type LinkedItems =
    fn(&rusqlite::Connection, &SpaceId, Option<LinkKind>) -> Result<Vec<ItemRef>>;

/// The items a watch of `kind`, or of every kind, follows: the assigned
/// issue and PRs, and the linked runs whose finish it has yet to deliver.
pub fn followed_items(
    db: &rusqlite::Connection,
    id: &SpaceId,
    kind: Option<LinkKind>,
) -> Result<Vec<ItemRef>> {
    with_runs(db, id, kind, unfinished_runs)
}

/// The items the background poll reads for integrations: the assigned issue
/// and PRs, and the linked runs its own baseline has not seen finish, apart
/// from what watches delivered.
pub fn polled_items(db: &rusqlite::Connection, id: &SpaceId) -> Result<Vec<ItemRef>> {
    with_runs(db, id, None, unpolled_runs)
}

/// The items `shoal view` shows for `kind`, or every kind: every linked
/// item, the assigned issue and PRs first.
pub fn shown_items(
    db: &rusqlite::Connection,
    id: &SpaceId,
    kind: Option<LinkKind>,
) -> Result<Vec<ItemRef>> {
    let links = crate::linked::space_links(db, id)?;
    Ok((links.items().into_iter())
        .filter(|item| kind.is_none_or(|kind| item.kind() == kind))
        .map(|item| item.url)
        .collect())
}

/// The assigned issue and PRs of `kind`, or of every kind, and the runs
/// `runs` lists when the kind takes runs.
fn with_runs(
    db: &rusqlite::Connection,
    id: &SpaceId,
    kind: Option<LinkKind>,
    runs: fn(&rusqlite::Connection, &SpaceId) -> Result<Vec<ItemRef>>,
) -> Result<Vec<ItemRef>> {
    let mut items = match kind {
        None => crate::linked::assigned_items(db, id, None)?,
        Some(kind) => match kind.item() {
            Some(kind) => crate::linked::assigned_items(db, id, Some(kind))?,
            None => Vec::new(),
        },
    };
    if matches!(kind, None | Some(LinkKind::Ci)) {
        items.extend(runs(db, id)?);
    }
    Ok(items)
}

/// Every run the workspace links, oldest first.
fn linked_runs(db: &rusqlite::Connection, id: &SpaceId) -> Result<Vec<ItemRef>> {
    Ok(db
        .prepare(
            "SELECT url FROM space_items WHERE space_id=?1 AND kind=?2
             ORDER BY created_at, rowid",
        )?
        .query_map(params![id, LinkKind::Ci], |row| row.get::<_, ItemRef>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The workspace's linked runs, except those whose finish the workspace's
/// cursor holds as delivered, which ends their watch.
fn unfinished_runs(db: &rusqlite::Connection, id: &SpaceId) -> Result<Vec<ItemRef>> {
    let mut runs = linked_runs(db, id)?;
    let mut delivered = Vec::new();
    for url in &runs {
        let record = cursor_record(db, id, url.url())?;
        if record.is_some_and(|record| record.pending.is_empty() && record.snapshot.run_finished())
        {
            delivered.push(url.clone());
        }
    }
    runs.retain(|url| !delivered.contains(url));
    Ok(runs)
}

/// The workspace's linked runs the background poll has yet to see finish,
/// or to queue the finish of on the workspace's cursor; the two are
/// recorded apart, so a run is polled until both have it.
fn unpolled_runs(db: &rusqlite::Connection, id: &SpaceId) -> Result<Vec<ItemRef>> {
    let mut runs = linked_runs(db, id)?;
    let mut finished = Vec::new();
    for url in &runs {
        let observed = observed_item(db, id, url.url())?;
        let queued = cursor_record(db, id, url.url())?;
        if observed.is_some_and(|snapshot| snapshot.run_finished())
            && queued.is_some_and(|record| record.snapshot.run_finished())
        {
            finished.push(url.clone());
        }
    }
    runs.retain(|url| !finished.contains(url));
    Ok(runs)
}

/// Whether the workspace watches a PR that no poll has seen merged or
/// closed, so its activity is still to come.
pub fn watches_open_pr(db: &rusqlite::Connection, id: &SpaceId) -> Result<bool> {
    Ok(!open_prs(db, id)?.is_empty())
}

/// The PRs the workspace watches that no poll has seen merged or closed.
fn open_prs(db: &rusqlite::Connection, id: &SpaceId) -> Result<Vec<ItemRef>> {
    let mut open = Vec::new();
    for url in crate::linked::assigned_items(db, id, Some(ItemKind::Pr))? {
        if observed_item(db, id, url.url())?.is_none_or(|snapshot| !snapshot.concluded()) {
            open.push(url);
        }
    }
    Ok(open)
}

/// The PRs the space `id` links as opened, and does not watch, that no
/// background poll has seen merged or closed. Where `[pr] watch_merge_runs`
/// is on, the poll reads them only to follow their merges.
pub fn opened_open_prs(db: &rusqlite::Connection, id: &SpaceId) -> Result<Vec<ItemRef>> {
    let watched = crate::linked::assigned_items(db, id, Some(ItemKind::Pr))?;
    let mut open = Vec::new();
    for item in crate::linked::recorded_items(db, id, Relation::Opened)? {
        let url = item.url;
        if url.kind() != LinkKind::Pr || watched.contains(&url) {
            continue;
        }
        if observed_item(db, id, url.url())?.is_none_or(|snapshot| !snapshot.concluded()) {
            open.push(url);
        }
    }
    Ok(open)
}

/// The item as the workspace's last poll saw it, if one has.
pub fn observed_item(
    db: &rusqlite::Connection,
    id: &SpaceId,
    url: &str,
) -> Result<Option<Snapshot>> {
    let record: Option<String> = db
        .query_row(
            "SELECT record FROM space_item_activity WHERE space_id=?1 AND url=?2",
            params![id, url],
            |row| row.get(0),
        )
        .optional()?;
    Ok(record
        .map(|record| serde_json::from_str(&record))
        .transpose()?)
}

/// Whether the workspace's watch saw the item's activity end, as
/// [`Snapshot::activity_ended`] decides.
pub fn activity_ended(
    db: &rusqlite::Connection,
    id: &SpaceId,
    url: &str,
    done: bool,
) -> Result<bool> {
    let record = cursor_record(db, id, url)?;
    Ok(record.is_some_and(|record| record.snapshot.activity_ended(done)))
}
