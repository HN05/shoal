//! Persist issue associations.
use crate::daemon::refusal::{Reason, Refusal};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::{
    daemon::{
        events::{self, EventKind},
        workspace::Manager,
    },
    forge::{ForgeRepo, items::link::ItemKind},
    model::Workspace,
};

#[derive(Debug, Serialize, Deserialize)]
pub struct Registration {
    pub url: String,
    pub error: Option<String>,
}

impl Manager {
    pub async fn issue_registration(&self, id: &str) -> Result<Option<Registration>> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                Ok(db
                    .query_row(
                        "SELECT url,error FROM workspace_issue WHERE workspace_id=?1",
                        [id],
                        |row| {
                            Ok(Registration {
                                url: row.get(0)?,
                                error: row.get(1)?,
                            })
                        },
                    )
                    .optional()?)
            })
            .await
    }

    /// Association is part of opening a workspace, before tracked setup starts.
    /// Linking the same issue again records a newly known title.
    pub async fn set_issue(
        &self,
        selector: &str,
        input: &str,
        title: Option<String>,
    ) -> Result<()> {
        let (workspace, _guard) = self.lock_pr_gate(selector).await?;
        self.set_issue_gated(&workspace.id, input, title).await
    }

    /// [`Self::set_issue`] for a caller already holding the workspace's PR
    /// gate.
    pub(crate) async fn set_issue_gated(
        &self,
        selector: &str,
        input: &str,
        title: Option<String>,
    ) -> Result<()> {
        let workspace = self.workspace(selector).await?;
        self.verify_worktree(&workspace).await?;
        let (_, _, url) = self.issue_forge(&workspace, input).await?;
        self.persist_issue_assignment(&workspace.id, url, title)
            .await
    }

    async fn persist_issue_assignment(
        &self,
        id: &str,
        url: String,
        title: Option<String>,
    ) -> Result<()> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                assign(&tx, &id, &url, title.as_deref())?;
                tx.commit()?;
                Ok(())
            })
            .await
    }

    pub async fn clear_issue(&self, selector: &str, input: Option<&str>) -> Result<()> {
        let (workspace, _guard) = self.lock_pr_gate(selector).await?;
        let selected = match input {
            Some(input) => Some(self.issue_forge(&workspace, input).await?.2),
            None => None,
        };
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                unlink(&tx, &workspace.id, selected.as_deref())?;
                tx.commit()?;
                Ok(())
            })
            .await?;
        self.signals.wake_cleanup();
        Ok(())
    }

    pub(super) async fn issue_forge(
        &self,
        workspace: &Workspace,
        input: &str,
    ) -> Result<(ForgeRepo, u64, String)> {
        let remote = crate::repository::remote_url_from_path(&workspace.path)
            .await?
            .context("issue lookup needs an origin remote")?;
        let forge = self.forge(&remote).await?;
        let (number, url) = forge.issue(input)?;
        Ok((forge, number, url))
    }
}

/// Assign an issue within the caller's transaction, retaining a known title
/// when none is supplied and recording only a new association as linked.
fn assign(db: &Connection, id: &str, url: &str, title: Option<&str>) -> Result<()> {
    super::ensure_accepts_links(db, id)?;
    let existing: Option<String> = db
        .query_row(
            "SELECT url FROM workspace_issue WHERE workspace_id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()?;
    ensure!(
        existing.as_deref().is_none_or(|existing| existing == url),
        "workspace is already associated with a different issue"
    );
    db.execute(
        "INSERT INTO workspace_issue(workspace_id,url,title) VALUES (?1,?2,?3)
         ON CONFLICT(workspace_id) DO UPDATE SET title=COALESCE(excluded.title,title)",
        rusqlite::params![id, url, title],
    )?;
    if existing.is_none() {
        events::record_link(
            db,
            id,
            EventKind::Linked,
            crate::model::Relation::Assigned,
            ItemKind::Issue,
            url,
        )?;
    }
    Ok(())
}

/// Remove the workspace's issue, which must be `selected` when one is named,
/// along with what its link kept.
fn unlink(db: &Connection, id: &str, selected: Option<&str>) -> Result<()> {
    super::ensure_accepts_links(db, id)?;
    let existing: Option<String> = db
        .query_row(
            "SELECT url FROM workspace_issue WHERE workspace_id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(url) = selected {
        ensure!(
            existing.as_deref() == Some(url),
            Refusal::new(Reason::NotFound, format!("issue is not linked: {url}"))
        );
    }
    if let Some(url) = existing {
        super::forget_assigned(db, id, ItemKind::Issue, &[url])?;
    }
    db.execute("DELETE FROM workspace_issue WHERE workspace_id=?1", [id])?;
    Ok(())
}

#[cfg(test)]
mod tests;
