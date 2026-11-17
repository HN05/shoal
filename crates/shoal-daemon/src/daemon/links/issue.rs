//! Persist issue associations.
use crate::refusal::{Reason, Refusal};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;
use std::path::Path;

use anyhow::{Context, Result, ensure};
use rusqlite::Connection;

pub(crate) mod agent_settles;
mod agents;
mod assignees;

pub(crate) use crate::model::{Assigner, IssueAssignee};
use crate::{
    daemon::{
        events::{self, EventKind},
        links::space::LinkSpace,
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
pub(crate) use assignees::{AssignerOf, Dropped, of_space as assignees_of};
use shoal_forge::{RepoRequests, items::issue::IssueRequests};
pub(crate) use shoal_protocol::links::issue::Registration;

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
    /// The issue joins the space's assigned issues as `assigner` assigns it;
    /// linking an assigned one again records a newly known title. An issue
    /// no assignee is recorded for, a new one or one whose assignment
    /// failed, is assigned on the forge; returns the warnings that gives.
    pub async fn set_issue(
        &self,
        selector: &SpaceSelector,
        input: &ItemInput,
        title: Option<String>,
        assigner: Assigner,
    ) -> Result<Vec<String>> {
        let id = self.link_space(selector).await?.id.to_owned();
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
        let fresh = self
            .persist_issue_assignment(&space.id, &url, title, assigner)
            .await?;
        (self.start_linked_watches_gated(&space.id, vec![url.clone()])).await;
        Ok(self.assign_linked(&space, &forge, &url, fresh).await)
    }

    /// Put the accounts its `[issue] assign` names on the issue `url`, of
    /// the repository of `forge`, which `space` just linked as assigned:
    /// the user's own login where the forge has yet to show the link, as
    /// `fresh` says, and the agent account while an agent works the issue.
    /// Returns what the caller hears: the issue's other assignees, or why
    /// an account was not assigned.
    async fn assign_linked(
        &self,
        space: &LinkSpace,
        forge: &ForgeRepo,
        url: &ItemRef,
        fresh: bool,
    ) -> Vec<String> {
        let assign = match self.space_settings(space).await {
            Ok(settings) => settings.issue.assign,
            Err(error) => return vec![assignees::unassigned(url, &error)],
        };
        let mut warnings = Vec::new();
        if fresh && assign.user() {
            warnings.extend(self.assign_on_forge(&space.id, forge, url).await);
        }
        if assign.agent() {
            // The user's assignment names the others already.
            let others = !assign.user();
            warnings.extend(self.assign_linked_agent(url, others).await);
        }
        warnings
    }

    /// Record the issue `url` as assigned to the space `id` by `assigner`,
    /// returning whether the forge has yet to show it as the space's.
    async fn persist_issue_assignment(
        &self,
        id: &SpaceId,
        url: &ItemRef,
        title: Option<String>,
        assigner: Assigner,
    ) -> Result<bool> {
        let (id, url) = (id.to_owned(), url.clone());
        self.store
            .write(move |tx| assign(tx, &id, url.url(), title.as_deref(), assigner))
            .await
    }

    /// Unlink the space's assigned issue `input` names, or every one, and
    /// take back the forge assignments that only it kept. Returns why an
    /// assignee stays or was not taken back.
    pub async fn clear_issue(
        &self,
        selector: &SpaceSelector,
        input: Option<&ItemInput>,
    ) -> Result<Vec<String>> {
        let id = self.link_space(selector).await?.id.to_owned();
        let _guard = self.gates.lock_prs([&id]).await;
        let space = self.link_space(&SpaceSelector::id(&id)).await?;
        let selected = match input {
            Some(input) => Some(self.issue_forge(space.origin_path(), input).await?.1),
            None => None,
        };
        let (removed, dropped) = self
            .store
            .write(move |tx| unlink(tx, &id, selected.as_ref().map(ItemRef::url)))
            .await?;
        self.signals.wake_cleanup();
        let mut warnings = self.release_assignees(dropped).await;
        warnings.extend(self.settle_issue_agents(&space, &removed).await);
        Ok(warnings)
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
/// linked. Once an agent assigns it, it stays an agent's. Returns whether
/// the forge has yet to show the link: no assignee is recorded for it, as
/// for a new one or one whose assignment failed.
fn assign(
    db: &Connection,
    id: &SpaceId,
    url: &str,
    title: Option<&str>,
    assigner: Assigner,
) -> Result<bool> {
    super::ensure_accepts_links(db, id)?;
    let assigned = assigned_urls(db, id)?
        .iter()
        .any(|issue| issue.url() == url);
    let assignee: Option<String> = db.query_row(
        "INSERT INTO space_issue(space_id,url,title,linked_by) VALUES (?1,?2,?3,?4)
         ON CONFLICT(space_id,url) DO UPDATE SET title=COALESCE(excluded.title,title),
            linked_by=CASE WHEN excluded.linked_by=?5 THEN ?5 ELSE linked_by END
         RETURNING assignee",
        rusqlite::params![id, url, title, assigner, Assigner::Agent],
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
/// the issues removed and the user's assignees the space stood for.
fn unlink(
    db: &Connection,
    id: &SpaceId,
    selected: Option<&str>,
) -> Result<(Vec<ItemRef>, Vec<Dropped>)> {
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
    Ok((removed, dropped))
}

/// The user's assignees the space `id` stands for on every issue it links
/// as assigned, which its removal drops.
pub(crate) fn removed_assignees(db: &Connection, id: &SpaceId) -> Result<Vec<Dropped>> {
    assignees::dropped(db, id, &assigned_urls(db, id)?)
}

/// The issues the space `id` links as assigned whose agent account its
/// removal settles: all but those it handed to the machine it moved to.
pub(crate) fn removed_issues(db: &Connection, id: &SpaceId) -> Result<Vec<ItemRef>> {
    agents::settled_here(db, id)
}

/// Record that an agent of the space `id` ended, within the caller's
/// transaction, so the issues an agent assigned it no longer count as
/// worked without one.
pub(crate) fn agent_ended(db: &Connection, id: &SpaceId) -> Result<()> {
    agents::agent_ended(db, id)
}

/// What the caller hears of the issue `url`'s assignees but the accounts
/// Shoal assigns: the `others`.
fn also_assigned(url: &ItemRef, others: &[String]) -> String {
    let others = others.join(", ");
    format!("{url} is also assigned to {others}")
}

/// The URLs of the space's assigned issues in the order they were assigned.
pub(crate) fn assigned_urls(db: &Connection, id: &SpaceId) -> Result<Vec<ItemRef>> {
    let issues = store::assigned_issues(db, id)?.into_iter();
    Ok(issues.map(|issue| issue.url).collect())
}

#[cfg(test)]
pub(in crate::daemon::links) mod test_support;
#[cfg(test)]
mod tests;
