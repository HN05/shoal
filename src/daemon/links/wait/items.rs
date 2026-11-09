//! The items a space's watches, background polls and views follow, and
//! the item an explicit selection names.
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};

use super::cursor::cursor_record;
use crate::{
    client::space_ids::SpaceId,
    daemon::{
        links::{
            pr::{Registration, RegistrationKind},
            space::LinkSpace,
        },
        workspace::Manager,
    },
    forge::{
        items::link::{ItemKind, LinkKind, Selection},
        updates::Snapshot,
    },
    model::Relation,
    repository,
};

pub(super) const NO_LINKED_ITEMS: &str =
    "no linked items of the selected kind; register one with shoal link";
impl Manager {
    /// The canonical URL of an explicitly selected item, linked or not, of
    /// the repository whose origin `checkout` has, or of a CI run of any.
    pub(crate) async fn explicit_item(
        &self,
        checkout: &std::path::Path,
        selection: &Selection,
    ) -> Result<Option<(String, LinkKind)>> {
        let Some(input) = &selection.input else {
            return Ok(None);
        };
        let remote = repository::remote_url_from_path(checkout)
            .await?
            .context("item lookup needs an origin remote")?;
        let forge = self.forge(&remote).await?;
        let kind = selection.kind.context("explicit items need a kind")?;
        // A run's URL may name another repository.
        let named = match kind {
            LinkKind::Ci => self.named_run_forge(input).await?,
            LinkKind::Issue | LinkKind::Pr => None,
        };
        let (_, url) = named.unwrap_or(forge).item(kind, input)?;
        Ok(Some((url, kind)))
    }

    /// The explicitly selected item, or the space's linked items of the
    /// selected kind, with every linked run.
    pub(crate) async fn selected_items(
        &self,
        space: &LinkSpace,
        selection: &Selection,
    ) -> Result<Vec<(String, LinkKind)>> {
        let items = self.selection_items(space, selection, shown_items).await?;
        ensure!(
            !items.is_empty() || selection.kind.is_none(),
            "{NO_LINKED_ITEMS}"
        );
        Ok(items)
    }

    /// The space's own items a watch selects. A kind it links none of is
    /// still watched when a child workspace whose activity its agent receives
    /// links one.
    pub(super) async fn watched_items(
        &self,
        space: &LinkSpace,
        selection: &Selection,
    ) -> Result<Vec<(String, LinkKind)>> {
        let items = self
            .selection_items(space, selection, followed_items)
            .await?;
        if items.is_empty() && selection.input.is_none() {
            let (id, kind) = (space.id().to_owned(), selection.kind);
            let agents = self.tracked_agents().await;
            ensure!(
                self.store
                    .run(move |db| links_kind(db, &id, kind, &agents))
                    .await?,
                "{NO_LINKED_ITEMS}"
            );
        }
        Ok(items)
    }

    /// The explicitly selected item, or the space's linked items of the
    /// selected kind that `linked` lists.
    pub(in crate::daemon::links) async fn selection_items(
        &self,
        space: &LinkSpace,
        selection: &Selection,
        linked: LinkedItems,
    ) -> Result<Vec<(String, LinkKind)>> {
        if let Some(item) = self.explicit_item(space.origin_path(), selection).await? {
            return Ok(vec![item]);
        }
        let id = space.id().to_owned();
        let kind = selection.kind;
        self.store.run(move |db| linked(db, &id, kind)).await
    }
}

/// Whether the workspace, or a child workspace whose activity its agent
/// receives, links an item of `kind`; every workspace has items of no kind.
pub(super) fn links_kind(
    db: &rusqlite::Connection,
    id: &SpaceId,
    kind: Option<LinkKind>,
    agents: &crate::daemon::workspace::TrackedAgents,
) -> Result<bool> {
    if kind.is_none() || !followed_items(db, id, kind)?.is_empty() {
        return Ok(true);
    }
    for child in crate::daemon::workspace::delegated(db, id, agents)? {
        if !followed_items(db, &child.id, kind)?.is_empty() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The workspace's items of a kind, or of every kind, that one use of its
/// links covers.
type LinkedItems =
    fn(&rusqlite::Connection, &SpaceId, Option<LinkKind>) -> Result<Vec<(String, LinkKind)>>;

/// The items a watch of `kind`, or of every kind, follows: the assigned
/// issue and PRs, the related PRs whose conclusion it has yet to deliver,
/// and the linked runs whose finish it has yet to deliver.
pub(crate) fn followed_items(
    db: &rusqlite::Connection,
    id: &SpaceId,
    kind: Option<LinkKind>,
) -> Result<Vec<(String, LinkKind)>> {
    with_links(db, id, kind, undelivered)
}

/// The items the background poll reads for integrations: the assigned issue
/// and PRs, and the related PRs and linked runs its own baseline has not
/// seen end, apart from what watches delivered.
pub(super) fn polled_items(
    db: &rusqlite::Connection,
    id: &SpaceId,
) -> Result<Vec<(String, LinkKind)>> {
    with_links(db, id, None, unpolled)
}

/// The assigned issue and PRs of `kind`, or of every kind, alone.
pub(crate) fn assigned_items(
    db: &rusqlite::Connection,
    id: &SpaceId,
    kind: Option<LinkKind>,
) -> Result<Vec<(String, LinkKind)>> {
    let kind = match kind {
        None => None,
        Some(kind) => match kind.item() {
            Some(kind) => Some(kind),
            None => return Ok(Vec::new()),
        },
    };
    Ok((linked_items(db, id, kind)?.into_iter())
        .map(|(url, kind)| (url, kind.into()))
        .collect())
}

/// The items `shoal view` shows for `kind`, or every kind: every linked
/// item once, the assigned issue and PRs first.
fn shown_items(
    db: &rusqlite::Connection,
    id: &SpaceId,
    kind: Option<LinkKind>,
) -> Result<Vec<(String, LinkKind)>> {
    let links = crate::daemon::store::space_links(db, id)?;
    Ok((links.items().into_iter())
        .filter(|item| kind.is_none_or(|kind| item.kind == kind))
        .map(|item| (item.url, item.kind))
        .collect())
}

/// Which of a space's linked runs or related PRs a use of its links still
/// reads, given the URLs and when one's activity has ended.
type Unended = fn(&rusqlite::Connection, &SpaceId, Vec<String>, Ended) -> Result<Vec<String>>;

/// Whether a snapshot shows the end of a linked run's or related PR's
/// activity.
type Ended = fn(&Snapshot) -> bool;

/// The assigned issue and PRs of `kind`, or of every kind, then the related
/// PRs and the runs, when the kind takes them, that `unended` keeps.
fn with_links(
    db: &rusqlite::Connection,
    id: &SpaceId,
    kind: Option<LinkKind>,
    unended: Unended,
) -> Result<Vec<(String, LinkKind)>> {
    let mut items = assigned_items(db, id, kind)?;
    if matches!(kind, None | Some(LinkKind::Pr)) {
        let related = concluding_prs(db, id)?;
        let open = unended(db, id, related, Snapshot::concluded)?;
        items.extend(open.into_iter().map(|url| (url, LinkKind::Pr)));
    }
    if matches!(kind, None | Some(LinkKind::Ci)) {
        let runs = unended(db, id, linked_runs(db, id)?, Snapshot::run_finished)?;
        items.extend(runs.into_iter().map(|url| (url, LinkKind::Ci)));
    }
    Ok(items)
}

/// The PRs the workspace links as related and not as assigned, which its
/// watches follow only until they merge or close.
pub(super) fn concluding_prs(db: &rusqlite::Connection, id: &SpaceId) -> Result<Vec<String>> {
    let assigned = linked_items(db, id, Some(ItemKind::Pr))?;
    let mut related = linked_urls(db, id, LinkKind::Pr, Some(Relation::Related))?;
    related.retain(|url| !assigned.iter().any(|(assigned, _)| assigned == url));
    Ok(related)
}

/// Every run the workspace links, oldest first, once.
fn linked_runs(db: &rusqlite::Connection, id: &SpaceId) -> Result<Vec<String>> {
    linked_urls(db, id, LinkKind::Ci, None)
}

/// Every item of `kind` the workspace links under `relation`, or under any,
/// oldest first, once.
fn linked_urls(
    db: &rusqlite::Connection,
    id: &SpaceId,
    kind: LinkKind,
    relation: Option<Relation>,
) -> Result<Vec<String>> {
    let urls = db
        .prepare(
            "SELECT url FROM space_items WHERE space_id=?1 AND kind=?2
             AND (?3 IS NULL OR relation=?3) ORDER BY created_at, rowid",
        )?
        .query_map(params![id, kind, relation], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut unique = Vec::new();
    for url in urls {
        if !unique.contains(&url) {
            unique.push(url);
        }
    }
    Ok(unique)
}

/// The `urls` except those whose end the workspace's cursor holds as
/// delivered, which ends their watch.
fn undelivered(
    db: &rusqlite::Connection,
    id: &SpaceId,
    mut urls: Vec<String>,
    ended: Ended,
) -> Result<Vec<String>> {
    let mut delivered = Vec::new();
    for url in &urls {
        let record = cursor_record(db, id, url)?;
        if record.is_some_and(|record| record.pending.is_empty() && ended(&record.snapshot)) {
            delivered.push(url.clone());
        }
    }
    urls.retain(|url| !delivered.contains(url));
    Ok(urls)
}

/// The `urls` the background poll has yet to see end, or to queue the end
/// of on the workspace's cursor; the two are recorded apart, so an item is
/// polled until both have it.
fn unpolled(
    db: &rusqlite::Connection,
    id: &SpaceId,
    mut urls: Vec<String>,
    ended: Ended,
) -> Result<Vec<String>> {
    let mut finished = Vec::new();
    for url in &urls {
        let observed = observed_item(db, id, url)?;
        let queued = cursor_record(db, id, url)?;
        if observed.is_some_and(|snapshot| ended(&snapshot))
            && queued.is_some_and(|record| ended(&record.snapshot))
        {
            finished.push(url.clone());
        }
    }
    urls.retain(|url| !finished.contains(url));
    Ok(urls)
}

/// The workspace's assigned issue and PRs, of `kind` or every kind.
pub(crate) fn linked_items(
    db: &rusqlite::Connection,
    id: &SpaceId,
    kind: Option<ItemKind>,
) -> Result<Vec<(String, ItemKind)>> {
    let mut items = Vec::new();
    if kind != Some(ItemKind::Issue) {
        let registration: Option<String> = db
            .query_row(
                "SELECT record FROM pr_cleanup WHERE space_id=?1",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(Registration {
            kind: RegistrationKind::Watch { urls, .. },
            ..
        }) = registration
            .map(|text| serde_json::from_str(&text))
            .transpose()?
        {
            items.extend(urls.into_iter().map(|url| (url, ItemKind::Pr)));
        }
    }
    if kind != Some(ItemKind::Pr) {
        let url: Option<String> = db
            .query_row(
                "SELECT url FROM space_issue WHERE space_id=?1",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        items.extend(url.into_iter().map(|url| (url, ItemKind::Issue)));
    }
    Ok(items)
}

/// Whether the workspace watches a PR that no poll has seen merged or
/// closed, so its activity is still to come.
pub(crate) fn watches_open_pr(db: &rusqlite::Connection, id: &SpaceId) -> Result<bool> {
    Ok(!open_prs(db, id)?.is_empty())
}

/// The PRs the workspace watches that no poll has seen merged or closed.
pub(crate) fn open_prs(db: &rusqlite::Connection, id: &SpaceId) -> Result<Vec<String>> {
    let mut open = Vec::new();
    for (url, _) in linked_items(db, id, Some(ItemKind::Pr))? {
        if observed_item(db, id, &url)?.is_none_or(|snapshot| !snapshot.concluded()) {
            open.push(url);
        }
    }
    Ok(open)
}

/// The item as the workspace's last poll saw it, if one has.
pub(super) fn observed_item(
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

/// Store `snapshot` as the item as the workspace's last poll saw it.
pub(super) fn save_observed_item(
    db: &rusqlite::Connection,
    id: &SpaceId,
    url: &str,
    snapshot: &Snapshot,
) -> Result<()> {
    db.execute(
        "INSERT INTO space_item_activity(space_id,url,record) VALUES (?1,?2,?3)
         ON CONFLICT(space_id,url) DO UPDATE SET record=excluded.record",
        params![id, url, serde_json::to_string(snapshot)?],
    )?;
    Ok(())
}

/// Whether the workspace's watch saw the item's activity end, as
/// [`Snapshot::activity_ended`] decides.
pub(super) fn activity_ended(
    db: &rusqlite::Connection,
    id: &SpaceId,
    url: &str,
    done: bool,
) -> Result<bool> {
    let record = cursor_record(db, id, url)?;
    Ok(record.is_some_and(|record| record.snapshot.activity_ended(done)))
}
