//! Link issues, PRs and CI runs to workspaces by relation. Assigned items
//! keep the issue and PR watch rules; opened and related items are only
//! recorded, and watches follow linked runs until they finish.
mod actions;
mod checks;
mod ci;
pub mod issue;
mod listing;
mod memories;
pub mod pr;
pub mod proposals;
mod raw;
mod revert;
mod run_approval;
mod runs;
mod stack;
pub mod wait;

pub(crate) use memories::ForgeMemories;

use crate::daemon::refusal::{Reason, Refusal};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension};

use crate::{
    daemon::{
        events::{self, EventKind},
        links::pr::Action,
        workspace::{Manager, review},
    },
    forge::items::link::{ItemKind, LinkKind},
    model::{LinkedItem, Relation},
    state::WorkspaceState,
};

impl Manager {
    pub async fn link_item(
        &self,
        selector: &str,
        kind: LinkKind,
        input: &str,
        relation: Relation,
        title: Option<String>,
    ) -> Result<()> {
        match (relation, kind) {
            (Relation::Assigned, LinkKind::Issue) => self.set_issue(selector, input, title).await,
            (Relation::Assigned, LinkKind::Pr) => {
                let url = input.to_owned();
                self.set_pr(selector, Action::Watch { url }).await
            }
            (Relation::Assigned, LinkKind::Ci) => bail!(
                "a CI run cannot be assigned, because assigned items drive completion \
                 and cleanup; link it as opened or related"
            ),
            (relation, LinkKind::Ci) => self.record_run(selector, relation, input).await,
            (relation, kind) => {
                self.record_item(selector, relation, kind, input, title)
                    .await
            }
        }
    }

    /// Unlink the relation's items of `kind`, or only the one `input` names;
    /// one must be linked.
    pub async fn unlink_items(
        &self,
        selector: &str,
        relation: Relation,
        kind: Option<LinkKind>,
        input: Option<&str>,
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
    /// forge shows it, with its workflow as its title.
    async fn record_run(&self, selector: &str, relation: Relation, input: &str) -> Result<()> {
        let (workspace, forge, account) = self.run_forge(selector).await?;
        let (_, url) = forge.run(input)?;
        let run = forge
            .view_run(&account, &url)
            .await
            .with_context(|| format!("could not read CI run {url}"))?;
        let item = LinkedItem {
            kind: LinkKind::Ci,
            url,
            title: run.workflow,
        };
        self.record_items(&workspace.id, vec![(relation, item)])
            .await
    }

    /// Record an opened or related item named by number or URL.
    async fn record_item(
        &self,
        selector: &str,
        relation: Relation,
        kind: LinkKind,
        input: &str,
        title: Option<String>,
    ) -> Result<()> {
        let workspace = self.workspace(selector).await?;
        self.verify_worktree(&workspace).await?;
        let url = self.item_url(&workspace, kind, input).await?;
        let item = LinkedItem { kind, url, title };
        self.record_items(&workspace.id, vec![(relation, item)])
            .await
    }

    /// Forget a relation's recorded items of `kind`, or the one `input` names,
    /// which must be recorded.
    async fn forget_items(
        &self,
        selector: &str,
        relation: Relation,
        kind: Option<LinkKind>,
        input: Option<&str>,
    ) -> Result<()> {
        let workspace = self.workspace(selector).await?;
        self.verify_worktree(&workspace).await?;
        let url = match (kind, input) {
            (Some(kind), Some(input)) => Some(self.item_url(&workspace, kind, input).await?),
            _ => None,
        };
        let removed = self
            .remove_items(&workspace.id, relation, kind, url.clone())
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
        id: &str,
        items: Vec<(Relation, LinkedItem)>,
    ) -> Result<()> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                for (relation, item) in &items {
                    let title = item.title.as_deref();
                    record(&tx, &id, *relation, item.kind, &item.url, title)?;
                }
                tx.commit()?;
                Ok(())
            })
            .await
    }

    /// Remove a relation's recorded items in one transaction, returning how
    /// many were removed.
    async fn remove_items(
        &self,
        id: &str,
        relation: Relation,
        kind: Option<LinkKind>,
        url: Option<String>,
    ) -> Result<usize> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                let removed = remove(&tx, &id, relation, kind, url.as_deref())?;
                tx.commit()?;
                Ok(removed)
            })
            .await
    }

    async fn unassign(
        &self,
        selector: &str,
        kind: Option<ItemKind>,
        input: Option<&str>,
    ) -> Result<()> {
        if input.is_none() {
            self.require_assigned(selector, kind).await?;
        }
        if kind != Some(ItemKind::Pr) {
            self.clear_issue(selector, input).await?;
        }
        if kind != Some(ItemKind::Issue) {
            let action = input.map_or(Action::Clear, |url| Action::Unwatch {
                url: url.to_owned(),
            });
            self.set_pr(selector, action).await?;
        }
        Ok(())
    }

    /// Fail unless an item of `kind` is assigned: the issue, or a PR watch or
    /// acknowledgement, which an unreadable registration counts as.
    async fn require_assigned(&self, selector: &str, kind: Option<ItemKind>) -> Result<()> {
        let id = self.workspace(selector).await?.id;
        let issue = kind != Some(ItemKind::Pr) && self.issue_registration(&id).await?.is_some();
        let pr =
            kind != Some(ItemKind::Issue) && !matches!(self.pr_registration(&id).await, Ok(None));
        ensure!(
            issue || pr,
            "{} linked as {}",
            nothing(kind.map(Into::into)),
            Relation::Assigned
        );
        Ok(())
    }

    /// The canonical URL of an issue, PR or CI run of the workspace's
    /// repository.
    async fn item_url(
        &self,
        workspace: &crate::model::Workspace,
        kind: LinkKind,
        input: &str,
    ) -> Result<String> {
        Ok(match kind {
            LinkKind::Issue => self.issue_forge(workspace, input).await?.2,
            LinkKind::Pr => self.pr_forge(workspace, input).await?.2,
            LinkKind::Ci => self.workspace_forge(workspace).await?.run(input)?.1,
        })
    }
}

/// The subject of an error that no item of `kind` is linked.
fn nothing(kind: Option<LinkKind>) -> String {
    match kind {
        Some(kind) => format!("no {kind} is"),
        None => "nothing is".into(),
    }
}

/// Record a relation that has no lifecycle effect. Recording it again keeps
/// the first record and fills in a newly known title.
pub(crate) fn record(
    db: &Connection,
    id: &str,
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
    let added = db.execute(
        "INSERT INTO workspace_items(workspace_id,relation,kind,url,title)
         VALUES (?1,?2,?3,?4,?5) ON CONFLICT DO NOTHING",
        rusqlite::params![id, relation, kind, url, title],
    )?;
    if added == 0 {
        db.execute(
            "UPDATE workspace_items SET title=COALESCE(?4,title)
             WHERE workspace_id=?1 AND relation=?2 AND url=?3",
            rusqlite::params![id, relation, url, title],
        )?;
    } else {
        events::record_link(db, id, EventKind::Linked, relation, kind, url)?;
    }
    Ok(())
}

/// Remove recorded items, returning how many were removed.
fn remove(
    db: &Connection,
    id: &str,
    relation: Relation,
    kind: Option<LinkKind>,
    url: Option<&str>,
) -> Result<usize> {
    ensure_accepts_links(db, id)?;
    let removed = db
        .prepare(
            "DELETE FROM workspace_items WHERE workspace_id=?1 AND relation=?2
             AND (?3 IS NULL OR kind=?3) AND (?4 IS NULL OR url=?4) RETURNING kind,url",
        )?
        .query_map(rusqlite::params![id, relation, kind, url], |row| {
            Ok((row.get::<_, LinkKind>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (kind, url) in &removed {
        events::record_link(db, id, EventKind::Unlinked, relation, *kind, url)?;
    }
    forget_unlinked_runs(db, id, &removed)?;
    Ok(removed.len())
}

/// Drop the replay cursors of the runs among `removed` that no relation
/// links any more, so linking one again watches it afresh.
fn forget_unlinked_runs(db: &Connection, id: &str, removed: &[(LinkKind, String)]) -> Result<()> {
    let mut unlinked = Vec::new();
    for (kind, url) in removed {
        let linked: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM workspace_items WHERE workspace_id=?1 AND url=?2)",
            rusqlite::params![id, url],
            |row| row.get(0),
        )?;
        if *kind == LinkKind::Ci && !linked {
            unlinked.push(url.clone());
        }
    }
    forget_activity(db, id, &unlinked)
}

/// Drop what assigned items kept while linked, their replay cursors and review
/// marks, and journal each as unlinked.
pub(crate) fn forget_assigned(
    db: &Connection,
    id: &str,
    kind: ItemKind,
    urls: &[String],
) -> Result<()> {
    forget_activity(db, id, urls)?;
    review::forget(db, id, urls)?;
    for url in urls {
        events::record_link(db, id, EventKind::Unlinked, Relation::Assigned, kind, url)?;
    }
    Ok(())
}

/// Drop the replay cursors of items that are no longer linked.
fn forget_activity(db: &Connection, id: &str, urls: &[String]) -> Result<()> {
    let urls = serde_json::to_string(urls)?;
    for table in ["pr_activity", "workspace_item_activity"] {
        db.execute(
            &format!(
                "DELETE FROM {table} WHERE workspace_id=?1
                 AND url IN (SELECT value FROM json_each(?2))"
            ),
            rusqlite::params![id, urls],
        )?;
    }
    Ok(())
}

/// Refuse changes while the workspace is removed or otherwise in transition,
/// so a removal record taken during removal holds its final links.
pub(crate) fn ensure_accepts_links(db: &Connection, id: &str) -> Result<()> {
    let state = state(db, id)?;
    ensure!(
        state.accepts_links(),
        "workspace cannot change links while {state}"
    );
    Ok(())
}

/// Whether the workspace still exists and its state takes link changes now.
pub(crate) fn accepts_links(db: &Connection, id: &str) -> Result<bool> {
    let state: Option<WorkspaceState> = db
        .query_row("SELECT state FROM workspaces WHERE id=?1", [id], |row| {
            row.get(0)
        })
        .optional()?;
    Ok(state.is_some_and(WorkspaceState::accepts_links))
}

fn state(db: &Connection, id: &str) -> Result<WorkspaceState> {
    Ok(
        db.query_row("SELECT state FROM workspaces WHERE id=?1", [id], |row| {
            row.get(0)
        })?,
    )
}

#[cfg(test)]
mod tests;
