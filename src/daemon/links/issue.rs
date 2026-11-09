//! Persist issue associations.
use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceSelector;
use crate::forge::numbers::IssueNumber;
use crate::refusal::{Reason, Refusal};
use std::path::Path;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension};

pub(crate) use crate::client::links::issue::Registration;
use crate::{
    daemon::{
        events::{self, EventKind},
        workspace::Manager,
    },
    forge::{ForgeRepo, items::link::ItemKind},
};

/// An open issue a new workspace is assigned.
#[derive(Debug, Clone)]
pub(crate) struct OpenIssue {
    pub url: String,
    pub title: String,
}

impl Manager {
    pub async fn issue_registration(&self, id: &SpaceId) -> Result<Option<Registration>> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                Ok(db
                    .query_row(
                        "SELECT url,error FROM space_issue WHERE space_id=?1",
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
        selector: &SpaceSelector,
        input: &str,
        title: Option<String>,
    ) -> Result<()> {
        let id = self.link_space(selector).await?.id().to_owned();
        let _guard = self.gates.lock_prs([&id]).await;
        self.set_issue_gated(&SpaceSelector::id(&id), input, title)
            .await
    }

    /// [`Self::set_issue`] for a caller already holding the space's PR gate.
    pub(crate) async fn set_issue_gated(
        &self,
        selector: &SpaceSelector,
        input: &str,
        title: Option<String>,
    ) -> Result<()> {
        let space = self.link_space(selector).await?;
        self.verify_space(&space).await?;
        let (_, _, url) = self.issue_forge(space.origin_path(), input).await?;
        self.persist_issue_assignment(space.id(), url, title).await
    }

    async fn persist_issue_assignment(
        &self,
        id: &SpaceId,
        url: String,
        title: Option<String>,
    ) -> Result<()> {
        let id = id.to_owned();
        self.store
            .write(move |tx| assign(tx, &id, &url, title.as_deref()))
            .await
    }

    pub async fn clear_issue(&self, selector: &SpaceSelector, input: Option<&str>) -> Result<()> {
        let id = self.link_space(selector).await?.id().to_owned();
        let _guard = self.gates.lock_prs([&id]).await;
        let space = self.link_space(&SpaceSelector::id(&id)).await?;
        let selected = match input {
            Some(input) => Some(self.issue_forge(space.origin_path(), input).await?.2),
            None => None,
        };
        self.store
            .write(move |tx| unlink(tx, &id, selected.as_deref()))
            .await?;
        self.signals.wake_cleanup();
        Ok(())
    }

    /// The canonical URL and title of the open issue `input` names in the
    /// repository whose origin `checkout` has, read with the user's logins,
    /// as `shoal start` reads it. A closed or unknown issue is refused.
    pub(crate) async fn read_open_issue(&self, checkout: &Path, input: &str) -> Result<OpenIssue> {
        let remote = crate::repository::remote_url_from_path(checkout)
            .await?
            .context("issue lookup needs an origin remote")?;
        let forge = self.forge(&remote).await?;
        let (number, url) = forge.issue(input)?;
        let issue = forge.issue_details(&self.user_account(), number).await?;
        Ok(OpenIssue {
            url,
            title: issue.item.title,
        })
    }

    /// The forge of the repository whose origin `checkout` has, with the
    /// number and canonical URL of the issue `input` names there.
    pub(super) async fn issue_forge(
        &self,
        checkout: &Path,
        input: &str,
    ) -> Result<(ForgeRepo, IssueNumber, String)> {
        let remote = crate::repository::remote_url_from_path(checkout)
            .await?
            .context("issue lookup needs an origin remote")?;
        let forge = self.forge(&remote).await?;
        let (number, url) = forge.issue(input)?;
        Ok((forge, number, url))
    }
}

/// Assign an issue within the caller's transaction, retaining a known title
/// when none is supplied and recording only a new association as linked.
fn assign(db: &Connection, id: &SpaceId, url: &str, title: Option<&str>) -> Result<()> {
    super::ensure_accepts_links(db, id)?;
    let existing: Option<String> = db
        .query_row(
            "SELECT url FROM space_issue WHERE space_id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()?;
    ensure!(
        existing.as_deref().is_none_or(|existing| existing == url),
        "another issue is already assigned"
    );
    db.execute(
        "INSERT INTO space_issue(space_id,url,title) VALUES (?1,?2,?3)
         ON CONFLICT(space_id) DO UPDATE SET title=COALESCE(excluded.title,title)",
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
fn unlink(db: &Connection, id: &SpaceId, selected: Option<&str>) -> Result<()> {
    super::ensure_accepts_links(db, id)?;
    let existing: Option<String> = db
        .query_row(
            "SELECT url FROM space_issue WHERE space_id=?1",
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
    db.execute("DELETE FROM space_issue WHERE space_id=?1", [id])?;
    Ok(())
}

#[cfg(test)]
mod tests;
