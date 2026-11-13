//! Link issues, PRs and CI runs to spaces, workspaces and chats, by
//! relation. Assigned items keep the issue and PR watch rules, which drive a
//! workspace's completion and cleanup; opened and related items are only
//! recorded, and watches follow linked runs until they finish.
mod actions;
mod checks;
mod ci;
pub mod issue;
mod listing;
mod memories;
mod merge_warning;
pub mod pr;
pub mod proposals;
mod pushes;
mod raw;
mod reads;
mod revert;
mod run_approval;
mod runs;
pub(crate) mod space;
mod stack;
mod view;
pub mod wait;

pub(crate) use memories::ForgeMemories;

use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceSelector;
use crate::refusal::{Reason, Refusal};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension};

use crate::{
    client::links::Linked,
    daemon::{
        events::{self, EventKind},
        links::{pr::Action, space::LinkSpace},
        scope::Caller,
        workspace::{Manager, review},
    },
    forge::items::{
        link::{ItemKind, LinkKind},
        reference::{ItemInput, ItemRef},
    },
    model::{LinkedItem, Relation},
    state::WorkspaceState,
};

impl Manager {
    /// Link the item `input` names with `relation`; an issue or PR linked
    /// without its title gets the one the forge shows, or else none, which
    /// the answer says why.
    pub async fn link_item(
        &self,
        selector: &SpaceSelector,
        kind: LinkKind,
        input: &ItemInput,
        relation: Relation,
        title: Option<String>,
        caller: Option<&Caller>,
    ) -> Result<Linked> {
        let (title, title_error) = match (kind.item(), title) {
            (Some(kind), None) => self.item_title(selector, kind, input, caller).await,
            (_, title) => (title, None),
        };
        let warnings = self
            .record_link(selector, kind, input, relation, title, caller)
            .await?;
        Ok(Linked {
            title_error,
            warnings,
        })
    }

    /// Record the link [`Self::link_item`] makes, with `title`, returning
    /// what assigning an issue on the forge warns of.
    async fn record_link(
        &self,
        selector: &SpaceSelector,
        kind: LinkKind,
        input: &ItemInput,
        relation: Relation,
        title: Option<String>,
        caller: Option<&Caller>,
    ) -> Result<Vec<String>> {
        let assigner = issue::Assigner::of(caller);
        match (relation, kind) {
            (Relation::Assigned, LinkKind::Issue) => {
                return self.set_issue(selector, input, title, assigner).await;
            }
            (Relation::Assigned, LinkKind::Pr) => {
                let url = input.clone();
                self.set_pr(selector, Action::Watch { url, title }).await
            }
            (Relation::Assigned, LinkKind::Ci) => bail!(
                "a CI run cannot be assigned, because assigned items drive completion \
                 and cleanup; link it as opened or related"
            ),
            (relation, LinkKind::Ci) => self.record_run(selector, relation, input, caller).await,
            (relation, kind) => {
                self.record_item(selector, relation, kind, input, title)
                    .await
            }
        }?;
        Ok(Vec::new())
    }

    /// The title of the issue or PR `input` names, which lists show for the
    /// space, or else why the forge could not be read.
    async fn item_title(
        &self,
        selector: &SpaceSelector,
        kind: ItemKind,
        input: &ItemInput,
        caller: Option<&Caller>,
    ) -> (Option<String>, Option<String>) {
        let read = async {
            let context = self.link_space(selector).await?.item_context();
            let forge = self.context_forge(&context).await?;
            let item = ItemRef::in_repository(&forge, kind.into(), input)?;
            let account = self.read_account(caller);
            let current = forge.read(&account, kind, item.number().get()).await?;
            anyhow::Ok(current.item.title)
        };
        match read.await {
            Ok(title) => (Some(title), None),
            Err(error) => (None, Some(format!("{error:#}"))),
        }
    }

    /// Unlink the relation's items of `kind`, or only the one `input` names;
    /// one must be linked.
    pub async fn unlink_items(
        &self,
        selector: &SpaceSelector,
        relation: Relation,
        kind: Option<LinkKind>,
        input: Option<&ItemInput>,
    ) -> Result<()> {
        ensure!(
            input.is_none() || kind.is_some(),
            "an item to unlink needs its kind"
        );
        if relation == Relation::Assigned {
            let kind = match kind.map(LinkKind::item) {
                None => None,
                Some(Some(kind)) => Some(kind),
                Some(None) => bail!("{} linked as {relation}", nothing(kind)),
            };
            return self.unassign(selector, kind, input).await;
        }
        self.forget_items(selector, relation, kind, input).await
    }

    /// Record an opened or related run named by number or URL, once the
    /// forge shows it, with its workflow as its title; workspace processes
    /// link only runs of their own repository.
    async fn record_run(
        &self,
        selector: &SpaceSelector,
        relation: Relation,
        input: &ItemInput,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let space = self.link_space(selector).await?;
        let context = space.item_context();
        let place = (self.run_input_place(Some(&context), input, caller)).await?;
        let url = ItemRef::in_repository(&place.forge, LinkKind::Ci, input)?;
        let run = (place.forge.view_run(&place.account, &url).await)
            .with_context(|| format!("could not read CI run {url}"))?;
        let item = LinkedItem {
            url,
            title: run.workflow,
        };
        self.record_items(space.id(), vec![(relation, item)]).await
    }

    /// Record an opened or related item named by number or URL.
    async fn record_item(
        &self,
        selector: &SpaceSelector,
        relation: Relation,
        kind: LinkKind,
        input: &ItemInput,
        title: Option<String>,
    ) -> Result<()> {
        let space = self.link_space(selector).await?;
        self.verify_space(&space).await?;
        let url = self.space_item(&space, kind, input).await?;
        let item = LinkedItem { url, title };
        self.record_items(space.id(), vec![(relation, item)]).await
    }

    /// Forget a relation's recorded items of `kind`, or the one `input` names,
    /// which must be recorded.
    async fn forget_items(
        &self,
        selector: &SpaceSelector,
        relation: Relation,
        kind: Option<LinkKind>,
        input: Option<&ItemInput>,
    ) -> Result<()> {
        let space = self.link_space(selector).await?;
        self.verify_space(&space).await?;
        let url = match (kind, input) {
            (Some(kind), Some(input)) => Some(self.space_item(&space, kind, input).await?),
            _ => None,
        };
        let removed = self
            .remove_items(space.id(), relation, kind, url.clone())
            .await?;
        match url {
            Some(url) => ensure!(
                removed > 0,
                Refusal::new(
                    Reason::NotFound,
                    format!("{url} is not linked as {relation}")
                )
            ),
            None => ensure!(
                removed > 0,
                Refusal::new(
                    Reason::NotFound,
                    format!("{} linked as {relation}", nothing(kind))
                )
            ),
        }
        Ok(())
    }

    /// Record opened or related items in one transaction.
    pub(crate) async fn record_items(
        &self,
        id: &SpaceId,
        items: Vec<(Relation, LinkedItem)>,
    ) -> Result<()> {
        let id = id.to_owned();
        self.store
            .write(move |tx| {
                for (relation, item) in &items {
                    let title = item.title.as_deref();
                    record(tx, &id, *relation, item.kind(), item.url.url(), title)?;
                }
                Ok(())
            })
            .await
    }

    /// Remove a relation's recorded items in one transaction, returning how
    /// many were removed.
    async fn remove_items(
        &self,
        id: &SpaceId,
        relation: Relation,
        kind: Option<LinkKind>,
        url: Option<ItemRef>,
    ) -> Result<usize> {
        let id = id.to_owned();
        self.store
            .write(move |tx| remove(tx, &id, relation, kind, url.as_ref().map(ItemRef::url)))
            .await
    }

    async fn unassign(
        &self,
        selector: &SpaceSelector,
        kind: Option<ItemKind>,
        input: Option<&ItemInput>,
    ) -> Result<()> {
        if input.is_none() {
            self.require_assigned(selector, kind).await?;
        }
        if kind != Some(ItemKind::Pr) {
            self.clear_issue(selector, input).await?;
        }
        if kind != Some(ItemKind::Issue) {
            let action = input.map_or(Action::Clear, |url| Action::Unwatch { url: url.clone() });
            self.set_pr(selector, action).await?;
        }
        Ok(())
    }

    /// Fail unless an item of `kind` is assigned: an issue, or a PR watch or
    /// acknowledgement, which an unreadable registration counts as.
    async fn require_assigned(
        &self,
        selector: &SpaceSelector,
        kind: Option<ItemKind>,
    ) -> Result<()> {
        let space = self.link_space(selector).await?;
        let id = space.id();
        let issue = kind != Some(ItemKind::Pr) && !self.issue_registrations(id).await?.is_empty();
        let pr =
            kind != Some(ItemKind::Issue) && !matches!(self.pr_registration(id).await, Ok(None));
        ensure!(
            issue || pr,
            Refusal::new(
                Reason::NotFound,
                format!(
                    "{} linked as {}",
                    nothing(kind.map(Into::into)),
                    Relation::Assigned
                )
            )
        );
        Ok(())
    }

    /// The issue or PR of the space's repository, or the CI run of any,
    /// that `input` names.
    async fn space_item(
        &self,
        space: &LinkSpace,
        kind: LinkKind,
        input: &ItemInput,
    ) -> Result<ItemRef> {
        let forge = match kind {
            // A run's URL may name another repository.
            LinkKind::Ci => match self.named_run_forge(input).await? {
                Some(forge) => forge,
                None => self.space_forge(space).await?,
            },
            LinkKind::Issue | LinkKind::Pr => {
                let missing = "item lookup needs an origin remote";
                self.guarded_forge(space.origin_path(), missing).await?
            }
        };
        ItemRef::in_repository(&forge, kind, input)
    }
}

/// The subject of an error that no item of `kind` is linked.
fn nothing(kind: Option<LinkKind>) -> String {
    match kind {
        Some(kind) => format!("no {kind} is"),
        None => "nothing is".into(),
    }
}

/// Record a relation that has no lifecycle effect, in place of the item's
/// other recorded relation; an assigned item keeps its assignment. Recording
/// it again keeps the first record and fills in a newly known title.
pub(crate) fn record(
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

/// Take `title`, which the forge reports for the item at `url`, as each
/// title the space records for it, so its links show the item's current
/// title.
pub(crate) fn refresh_titles(db: &Connection, id: &SpaceId, url: &str, title: &str) -> Result<()> {
    for table in ["space_issue", "space_items"] {
        db.execute(
            &format!(
                "UPDATE {table} SET title=?3 WHERE space_id=?1 AND url=?2 AND title IS NOT ?3"
            ),
            rusqlite::params![id, url, title],
        )?;
    }
    pr::titles::refresh(db, id, url, title)
}

/// Refuse to record another relation of an item the space assigns.
fn ensure_unassigned(db: &Connection, id: &SpaceId, kind: LinkKind, url: &str) -> Result<()> {
    let Some(kind) = kind.item() else {
        return Ok(());
    };
    let assigned = wait::linked_items(db, id, Some(kind))?;
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
pub(crate) fn drop_recorded(
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
fn remove(
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

/// Drop what assigned items kept while linked, their replay cursors and review
/// marks, and journal each as unlinked.
pub(crate) fn forget_assigned(
    db: &Connection,
    id: &SpaceId,
    kind: ItemKind,
    urls: &[ItemRef],
) -> Result<()> {
    let urls: Vec<String> = urls.iter().map(ToString::to_string).collect();
    forget_activity(db, id, &urls)?;
    review::forget(db, id, &urls)?;
    for url in &urls {
        events::record_link(db, id, EventKind::Unlinked, Relation::Assigned, kind, url)?;
    }
    Ok(())
}

/// Drop the replay cursors of items that are no longer linked.
fn forget_activity(db: &Connection, id: &SpaceId, urls: &[String]) -> Result<()> {
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
pub(crate) fn ensure_accepts_links(db: &Connection, id: &SpaceId) -> Result<()> {
    if space::is_chat(db, id)? {
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
pub(crate) fn accepts_links(db: &Connection, id: &SpaceId) -> Result<bool> {
    if space::is_chat(db, id)? {
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

#[cfg(test)]
mod tests;
