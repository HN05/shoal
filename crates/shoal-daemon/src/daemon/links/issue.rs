//! Persist issue associations.
use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceSelector;
use crate::refusal::{Reason, Refusal};
use std::path::Path;

use anyhow::{Context, Result, ensure};
use rusqlite::Connection;

mod assignees;

pub(crate) use crate::client::links::issue::Registration;
use crate::{
    daemon::{
        events::{self, EventKind},
        store,
        workspace::Manager,
    },
    forge::{
        ForgeRepo,
        items::{
            link::{ItemKind, LinkKind},
            reference::{ItemInput, ItemRef},
        },
    },
};
pub(crate) use assignees::{Assigner, Dropped};

/// An open issue a new workspace is assigned.
#[derive(Debug, Clone)]
pub(crate) struct OpenIssue {
    pub url: ItemRef,
    pub title: String,
    pub labels: Vec<String>,
}

impl Manager {
    /// The space's assigned issues in the order they were assigned.
    pub async fn issue_registrations(&self, id: &SpaceId) -> Result<Vec<Registration>> {
        let id = id.to_owned();
        self.store
            .read(move |tx| {
                let issues = store::assigned_issues(tx, &id)?.into_iter();
                Ok(issues
                    .map(|issue| Registration {
                        url: issue.url,
                        error: issue.error,
                    })
                    .collect())
            })
            .await
    }

    /// Association is part of opening a workspace, before tracked setup starts.
    /// The issue joins the space's assigned issues; linking an assigned one
    /// again records a newly known title. An issue no assignee is recorded
    /// for, a new one or one whose assignment failed, is assigned on the
    /// forge to the account of `assigner`; returns the warnings that gives.
    pub async fn set_issue(
        &self,
        selector: &SpaceSelector,
        input: &ItemInput,
        title: Option<String>,
        assigner: Assigner,
    ) -> Result<Vec<String>> {
        let id = self.link_space(selector).await?.id().to_owned();
        let _guard = self.gates.lock_prs([&id]).await;
        self.set_issue_gated(&SpaceSelector::id(&id), input, title, assigner)
            .await
    }

    /// [`Self::set_issue`] for a caller already holding the space's PR gate.
    pub(crate) async fn set_issue_gated(
        &self,
        selector: &SpaceSelector,
        input: &ItemInput,
        title: Option<String>,
        assigner: Assigner,
    ) -> Result<Vec<String>> {
        let space = self.link_space(selector).await?;
        self.verify_space(&space).await?;
        let (forge, url) = self.issue_forge(space.origin_path(), input).await?;
        if !self
            .persist_issue_assignment(space.id(), &url, title)
            .await?
        {
            // The forge shows it already.
            return Ok(Vec::new());
        }
        Ok(self.assign_on_forge(&space, &forge, &url, assigner).await)
    }

    /// Record the issue `url` as assigned to the space `id`, returning
    /// whether the forge has yet to show it as the space's.
    async fn persist_issue_assignment(
        &self,
        id: &SpaceId,
        url: &ItemRef,
        title: Option<String>,
    ) -> Result<bool> {
        let (id, url) = (id.to_owned(), url.clone());
        self.store
            .write(move |tx| assign(tx, &id, url.url(), title.as_deref()))
            .await
    }

    /// Unlink the space's assigned issue `input` names, or every one, and
    /// take back the forge assignments that only it kept. Returns why an
    /// assignee stays.
    pub async fn clear_issue(
        &self,
        selector: &SpaceSelector,
        input: Option<&ItemInput>,
    ) -> Result<Vec<String>> {
        let id = self.link_space(selector).await?.id().to_owned();
        let _guard = self.gates.lock_prs([&id]).await;
        let space = self.link_space(&SpaceSelector::id(&id)).await?;
        let selected = match input {
            Some(input) => Some(self.issue_forge(space.origin_path(), input).await?.1),
            None => None,
        };
        let dropped = self
            .store
            .write(move |tx| unlink(tx, &id, selected.as_ref().map(ItemRef::url)))
            .await?;
        self.signals.wake_cleanup();
        Ok(self.release_assignees(dropped).await)
    }

    /// The canonical URL, title and labels of the open issue `input` names in the
    /// repository whose origin `checkout` has, read with the user's logins,
    /// as `shoal start` reads it. A closed or unknown issue is refused.
    pub(crate) async fn read_open_issue(
        &self,
        checkout: &Path,
        input: &ItemInput,
    ) -> Result<OpenIssue> {
        let (forge, url) = self.issue_forge(checkout, input).await?;
        let issue = forge
            .open_issue_details(&self.user_account(), url.issue()?)
            .await?;
        Ok(OpenIssue {
            url,
            title: issue.item.title,
            labels: issue.item.labels,
        })
    }

    /// The title of the PR `input` names in the repository whose origin
    /// `checkout` has.
    pub(crate) async fn read_pull_title(
        &self,
        checkout: &Path,
        input: &ItemInput,
    ) -> Result<String> {
        let remote = crate::repository::remote_url_from_path(checkout)
            .await?
            .context("PR lookup needs an origin remote")?;
        let forge = self.forge(&remote).await?;
        let pull = ItemRef::in_repository(&forge, LinkKind::Pr, input)?;
        Ok(forge.pull_request(&self.user_account(), &pull).await?.title)
    }

    /// The forge of the repository whose origin `checkout` has, with the
    /// number and canonical URL of the issue `input` names there.
    pub(super) async fn issue_forge(
        &self,
        checkout: &Path,
        input: &ItemInput,
    ) -> Result<(ForgeRepo, ItemRef)> {
        let remote = crate::repository::remote_url_from_path(checkout)
            .await?
            .context("issue lookup needs an origin remote")?;
        let forge = self.forge(&remote).await?;
        let issue = ItemRef::in_repository(&forge, LinkKind::Issue, input)?;
        Ok((forge, issue))
    }
}

/// Assign an issue within the caller's transaction, after those already
/// assigned and in place of its opened or related link, retaining a known
/// title when none is supplied and recording only a new association as
/// linked. Returns whether the forge has yet to show the link: no assignee
/// is recorded for it, as for a new one or one whose assignment failed.
fn assign(db: &Connection, id: &SpaceId, url: &str, title: Option<&str>) -> Result<bool> {
    super::ensure_accepts_links(db, id)?;
    let assigned = assigned_urls(db, id)?
        .iter()
        .any(|issue| issue.url() == url);
    let assignee: Option<String> = db.query_row(
        "INSERT INTO space_issue(space_id,url,title) VALUES (?1,?2,?3)
         ON CONFLICT(space_id,url) DO UPDATE SET title=COALESCE(excluded.title,title)
         RETURNING assignee",
        rusqlite::params![id, url, title],
        |row| row.get(0),
    )?;
    if !assigned {
        super::drop_recorded(db, id, url, None)?;
        events::record_link(
            db,
            id,
            EventKind::Linked,
            crate::model::Relation::Assigned,
            ItemKind::Issue,
            url,
        )?;
    }
    Ok(assignee.is_none())
}

/// Remove the space's issue `selected` names, which must be assigned, or
/// else every assigned issue, along with what their links kept, returning
/// the assignees the space stood for.
fn unlink(db: &Connection, id: &SpaceId, selected: Option<&str>) -> Result<Vec<Dropped>> {
    super::ensure_accepts_links(db, id)?;
    let mut removed = assigned_urls(db, id)?;
    if let Some(url) = selected {
        removed.retain(|issue| issue.url() == url);
        ensure!(
            !removed.is_empty(),
            Refusal::new(Reason::NotFound, format!("issue is not linked: {url}"))
        );
    }
    super::forget_assigned(db, id, ItemKind::Issue, &removed)?;
    let dropped = assignees::dropped(db, id, &removed)?;
    for url in &removed {
        db.execute(
            "DELETE FROM space_issue WHERE space_id=?1 AND url=?2",
            rusqlite::params![id, url.url()],
        )?;
    }
    Ok(dropped)
}

/// The assignees the space `id` stands for on every issue it links as
/// assigned, which its removal drops.
pub(crate) fn removed_assignees(db: &Connection, id: &SpaceId) -> Result<Vec<Dropped>> {
    assignees::dropped(db, id, &assigned_urls(db, id)?)
}

/// The URLs of the space's assigned issues in the order they were assigned.
pub(crate) fn assigned_urls(db: &Connection, id: &SpaceId) -> Result<Vec<ItemRef>> {
    let issues = store::assigned_issues(db, id)?.into_iter();
    Ok(issues.map(|issue| issue.url).collect())
}

#[cfg(test)]
mod tests;
