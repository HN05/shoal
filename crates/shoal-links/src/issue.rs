//! The issues a space assigns, as its records hold them.
pub mod agents;
pub mod assignees;

use anyhow::{Result, ensure};
use rusqlite::Connection;
use shoal_core::refusal::{Reason, Refusal};
use shoal_daemon_base::events::{self, EventKind};
use shoal_forge_types::items::{link::ItemKind, reference::ItemRef};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    links::issue::Registration,
    model::{Assigner, IssueAssignee, Relation},
};

use crate::Links;
use assignees::Dropped;

/// The forge assignees the space `id`'s assigned issues stand for, in the
/// order the issues were assigned: the user's login it stands for, and the
/// agent account for each issue an agent assigned it.
pub fn assignees_of(db: &Connection, id: &SpaceId) -> rusqlite::Result<Vec<IssueAssignee>> {
    db.prepare(
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
