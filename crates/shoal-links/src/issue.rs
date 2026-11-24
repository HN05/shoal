//! The issues a space assigns, as its records hold them.
pub mod agent_settles;
pub mod agents;
pub mod assignees;

use std::sync::Arc;

use anyhow::{Result, ensure};
use rusqlite::Connection;
use shoal_core::refusal::{Reason, Refusal};
use shoal_daemon_base::events::{self, EventKind};
use shoal_forge_types::{
    ForgeRepo,
    items::{
        link::ItemKind,
        reference::{ItemInput, ItemRef},
    },
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    links::issue::Registration,
    model::{Assigner, IssueAssignee, Relation},
    space_names::SpaceSelector,
};

use crate::{Links, daemon::Daemon, space::LinkSpace};
use assignees::Dropped;

/// The forge assignees the space `id`'s assigned issues stand for, in the
/// order the issues were assigned: the user's login it stands for, and the
/// agent account for each issue an agent assigned it.
pub fn assignees_of(db: &Connection, id: &SpaceId) -> rusqlite::Result<Vec<IssueAssignee>> {
    db.prepare_cached(
        "SELECT url,?2,EXISTS(SELECT 1 FROM issue_assignments a
             WHERE a.url=s.url AND a.login=s.assignee), rowid
         FROM space_issue s WHERE space_id=?1 AND assignee IS NOT NULL
         UNION ALL SELECT url,linked_by,0,rowid FROM space_issue
         WHERE space_id=?1 AND linked_by=?3 ORDER BY 4",
    )?
    .query_map(
        rusqlite::params![id, Assigner::User, Assigner::Agent],
        |row| {
            Ok(IssueAssignee {
                url: row.get(0)?,
                account: row.get(1)?,
                added: row.get(2)?,
            })
        },
    )?
    .collect()
}

impl Links {
    /// Association is part of opening a workspace, before tracked setup starts.
    /// The issue joins the space's assigned issues as `assigner` assigns it;
    /// linking an assigned one again records a newly known title. An issue
    /// no assignee is recorded for, a new one or one whose assignment
    /// failed, is assigned on the forge; returns the warnings that gives.
    pub async fn set_issue(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        input: &ItemInput,
        title: Option<String>,
        assigner: Assigner,
    ) -> Result<Vec<String>> {
        let id = self.link_space(daemon, selector).await?.id.to_owned();
        let _guard = self.gates.lock_prs([&id]).await;
        self.set_issue_gated(daemon, &SpaceSelector::id(&id), input, title, assigner)
            .await
    }

    /// [`Self::set_issue`] for a caller already holding the space's PR gate.
    pub async fn set_issue_gated(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        input: &ItemInput,
        title: Option<String>,
        assigner: Assigner,
    ) -> Result<Vec<String>> {
        let space = self.link_space(daemon, selector).await?;
        daemon.verify_space(&space).await?;
        let (forge, url) = self.issue_forge(space.origin_path(), input).await?;
        let fresh = self
            .persist_issue_assignment(&space.id, &url, title, assigner)
            .await?;
        (self.start_linked_watches_gated(daemon, &space.id, vec![url.clone()])).await;
        Ok(self
            .assign_linked(daemon, &space, &forge, &url, fresh)
            .await)
    }

    /// Put the accounts its `[issue] assign` names on the issue `url`, of
    /// the repository of `forge`, which `space` just linked as assigned:
    /// the user's own login where the forge has yet to show the link, as
    /// `fresh` says, and the agent account while an agent works the issue.
    /// Returns what the caller hears: the issue's other assignees, or why
    /// an account was not assigned.
    async fn assign_linked(
        &self,
        daemon: &dyn Daemon,
        space: &LinkSpace,
        forge: &ForgeRepo,
        url: &ItemRef,
        fresh: bool,
    ) -> Vec<String> {
        let assign = match daemon.acting_settings(space).await {
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
            warnings.extend(self.assign_linked_agent(daemon, url, others).await);
        }
        warnings
    }

    /// Record the issue `url` as assigned to the space `id` by `assigner`,
    /// returning whether the forge has yet to show it as the space's.
    async fn persist_issue_assignment(
        self: &Arc<Self>,
        id: &SpaceId,
        url: &ItemRef,
        title: Option<String>,
        assigner: Assigner,
    ) -> Result<bool> {
        let (id, url, links) = (id.to_owned(), url.clone(), self.clone());
        self.store
            .write(move |tx| links.assign(tx, &id, url.url(), title.as_deref(), assigner))
            .await
    }

    /// Unlink the space's assigned issue `input` names, or every one, and
    /// take back the forge assignments that only it kept. Returns why an
    /// assignee stays or was not taken back.
    pub async fn clear_issue(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        input: Option<&ItemInput>,
    ) -> Result<Vec<String>> {
        let id = self.link_space(daemon, selector).await?.id.to_owned();
        let _guard = self.gates.lock_prs([&id]).await;
        let space = self.link_space(daemon, &SpaceSelector::id(&id)).await?;
        let selected = match input {
            Some(input) => Some(self.issue_forge(space.origin_path(), input).await?.1),
            None => None,
        };
        let links = self.clone();
        let (removed, dropped) = self
            .store
            .write(move |tx| links.unlink(tx, &id, selected.as_ref().map(ItemRef::url)))
            .await?;
        daemon.wake_cleanup();
        let mut warnings = self.release_assignees(dropped).await;
        warnings.extend(self.settle_issue_agents(daemon, &space, &removed).await);
        Ok(warnings)
    }

    /// The space's assigned issues in the order they were assigned.
    pub async fn issue_registrations(&self, id: &SpaceId) -> Result<Vec<Registration>> {
        let id = id.to_owned();
        self.store
            .read(move |tx| {
                let issues = crate::linked::assigned_issues(tx, &id)?.into_iter();
                Ok(issues
                    .map(|issue| Registration {
                        url: issue.url,
                        error: issue.error,
                    })
                    .collect())
            })
            .await
    }

    /// Assign an issue within the caller's transaction, after those already
    /// assigned and in place of its opened or related link, retaining a known
    /// title when none is supplied and recording only a new association as
    /// linked. Once an agent assigns it, it stays an agent's. Returns whether
    /// the forge has yet to show the link: no assignee is recorded for it, as
    /// for a new one or one whose assignment failed.
    pub fn assign(
        &self,
        db: &Connection,
        id: &SpaceId,
        url: &str,
        title: Option<&str>,
        assigner: Assigner,
    ) -> Result<bool> {
        self.records.ensure_accepts_links(db, id)?;
        let assigned = crate::linked::assigned_urls(db, id)?
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
            crate::records::drop_recorded(db, id, url, None)?;
            events::record_link(
                db,
                id,
                EventKind::Linked,
                Relation::Assigned,
                ItemKind::Issue,
                url,
            )?;
        }
        Ok(assignee.is_none())
    }

    /// Remove the space's issue `selected` names, which must be assigned, or
    /// else every assigned issue, along with what their links kept, returning
    /// the issues removed and the user's assignees the space stood for.
    pub fn unlink(
        &self,
        db: &Connection,
        id: &SpaceId,
        selected: Option<&str>,
    ) -> Result<(Vec<ItemRef>, Vec<Dropped>)> {
        self.records.ensure_accepts_links(db, id)?;
        let mut removed = crate::linked::assigned_urls(db, id)?;
        if let Some(url) = selected {
            removed.retain(|issue| issue.url() == url);
            ensure!(
                !removed.is_empty(),
                Refusal::new(Reason::NotFound, format!("issue is not linked: {url}"))
            );
        }
        self.forget_assigned(db, id, ItemKind::Issue, &removed)?;
        let dropped = assignees::dropped(db, id, &removed)?;
        for url in &removed {
            db.execute(
                "DELETE FROM space_issue WHERE space_id=?1 AND url=?2",
                rusqlite::params![id, url.url()],
            )?;
        }
        Ok((removed, dropped))
    }
}
