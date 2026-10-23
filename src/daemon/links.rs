//! Link issues and PRs to workspaces by relation. Assigned items keep the
//! issue and PR watch rules; opened and related items are only recorded.
mod actions;
mod checks;
mod ci;
pub mod issue;
pub mod pr;
mod stack;
pub mod wait;

use anyhow::{Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension};

use crate::{
    daemon::{
        events::{self, EventKind},
        links::pr::Action,
        workspace::Manager,
    },
    forge::items::link::ItemKind,
    model::{LinkedItem, Relation},
    state::WorkspaceState,
};

impl Manager {
    pub async fn link_item(
        &self,
        selector: &str,
        kind: ItemKind,
        input: &str,
        relation: Relation,
        title: Option<String>,
    ) -> Result<()> {
        match (relation, kind) {
            (Relation::Assigned, ItemKind::Issue) => self.set_issue(selector, input, title).await,
            (Relation::Assigned, ItemKind::Pr) => {
                let url = input.to_owned();
                self.set_pr(selector, Action::Watch { url }).await
            }
            (relation, kind) => {
                self.record_item(selector, relation, kind, input, title)
                    .await
            }
        }
    }

    /// Unlink the relation's items of `kind`, or only the one `input` names.
    pub async fn unlink_items(
        &self,
        selector: &str,
        relation: Relation,
        kind: Option<ItemKind>,
        input: Option<&str>,
    ) -> Result<()> {
        ensure!(
            input.is_none() || kind.is_some(),
            "an item to unlink needs its kind"
        );
        if relation == Relation::Assigned {
            return self.unassign(selector, kind, input).await;
        }
        self.forget_items(selector, relation, kind, input).await
    }

    /// Record an opened or related item named by number or URL.
    async fn record_item(
        &self,
        selector: &str,
        relation: Relation,
        kind: ItemKind,
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
        kind: Option<ItemKind>,
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
        if let Some(url) = url {
            ensure!(removed > 0, "{url} is not linked as {relation}");
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
        kind: Option<ItemKind>,
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

    /// The canonical URL of an issue or PR of the workspace's repository.
    async fn item_url(
        &self,
        workspace: &crate::model::Workspace,
        kind: ItemKind,
        input: &str,
    ) -> Result<String> {
        Ok(match kind {
            ItemKind::Issue => self.issue_forge(workspace, input).await?.2,
            ItemKind::Pr => self.pr_forge(workspace, input).await?.2,
        })
    }
}

/// Record a relation that has no lifecycle effect. Recording it again keeps
/// the first record and fills in a newly known title.
pub(crate) fn record(
    db: &Connection,
    id: &str,
    relation: Relation,
    kind: ItemKind,
    url: &str,
    title: Option<&str>,
) -> Result<()> {
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
    kind: Option<ItemKind>,
    url: Option<&str>,
) -> Result<usize> {
    ensure_accepts_links(db, id)?;
    let removed = db
        .prepare(
            "DELETE FROM workspace_items WHERE workspace_id=?1 AND relation=?2
             AND (?3 IS NULL OR kind=?3) AND (?4 IS NULL OR url=?4) RETURNING kind,url",
        )?
        .query_map(rusqlite::params![id, relation, kind, url], |row| {
            Ok((row.get::<_, ItemKind>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (kind, url) in &removed {
        events::record_link(db, id, EventKind::Unlinked, relation, *kind, url)?;
    }
    Ok(removed.len())
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
