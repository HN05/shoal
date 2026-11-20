//! The user's own login among the forge assignees of the issues spaces link
//! as assigned: it joins them on each link where `[issue] assign` says so,
//! whoever links the issue, and leaves them once no space links the issue
//! for it, if Shoal added it. The records hold who stands for it and
//! whether Shoal added it.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use shoal_daemon_base::{
    alerts::{About, NewAlert},
    callers::Caller,
    log,
};
use shoal_forge::{
    items::assignment::{Assignment, AssignmentRequests},
    updates::Edited,
};
use shoal_forge_types::{ForgeRepo, items::reference::ItemRef};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::alerts::AlertKind;
use shoal_protocol::model::{Assigner, IssueAssignee};

use crate::{Links, daemon::Daemon, wait::changes::record_shoal_change};

/// Who a caller assigns an issue to a space as.
pub trait AssignerOf {
    /// An agent for a scoped `caller`, else the user.
    fn of(caller: Option<&Caller>) -> Self;
}

impl AssignerOf for Assigner {
    fn of(caller: Option<&Caller>) -> Self {
        match caller {
            Some(_) => Self::Agent,
            None => Self::User,
        }
    }
}

/// The login a space stood for among an issue's assignees, until it
/// dropped the issue's link.
#[derive(Debug, Clone)]
pub struct Dropped {
    pub url: String,
    pub login: String,
}

impl Links {
    /// Assign the issue `url`, of the repository of `forge`, which the
    /// space `id` just linked as assigned, to the user's own login. Returns
    /// what the caller hears: the issue's other assignees, or why it was
    /// not assigned.
    pub async fn assign_on_forge(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        forge: &ForgeRepo,
        url: &ItemRef,
    ) -> Vec<String> {
        match (self.assign_recorded(daemon, id, forge, url)).await {
            Ok(Some(assignment)) if !assignment.others.is_empty() => {
                vec![also_assigned(url, &assignment.others)]
            }
            Ok(_) => Vec::new(),
            Err(error) => vec![unassigned(url, &error)],
        }
    }

    /// Assign the issue `url` on `forge` to the user's own login, and
    /// record the login for the space `id` and, once Shoal added it, as
    /// Shoal's. A space that dropped the link meanwhile releases the login
    /// again.
    async fn assign_recorded(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        forge: &ForgeRepo,
        url: &ItemRef,
    ) -> Result<Option<Assignment>> {
        let gate = self.gates.lock_issue_assignees(url.url()).await;
        let assigned = forge.assign_own(&self.user_account(), url).await?;
        let Some(assignment) = assigned else {
            return Ok(None);
        };
        (self.record_assignee_change(url, assignment.edited.clone())).await;
        let (id, issue, login) = (id.to_owned(), url.clone(), assignment.login.clone());
        let added = assignment.added;
        let dropped = (self.store)
            .write(move |tx| record_assignee(tx, &id, issue.url(), &login, added))
            .await?;
        if let Some(dropped) = dropped {
            drop(gate);
            for warning in self.release_assignees(daemon, vec![dropped]).await {
                let alert = NewAlert::shoal(AlertKind::UnassignFailed, About::default(), warning);
                self.notifications.notify_user(alert).await;
            }
        }
        Ok(Some(assignment))
    }

    /// Hand the assignees the space `id` stands for to the machine it moved
    /// to: forget them for it, and Shoal's assignments no other space here
    /// links, so ending it here takes none off the forge.
    pub async fn hand_over_assignees(&self, id: &SpaceId) -> Result<()> {
        let id = id.to_owned();
        self.store.write(move |tx| hand_over(tx, &id)).await
    }

    /// Take on the user's assignments `assignees` says Shoal added, which a
    /// restored or moved space `id` carries, so its removal takes them back.
    pub async fn claim_assignees(&self, id: &SpaceId, assignees: Vec<IssueAssignee>) -> Result<()> {
        let id = id.to_owned();
        self.store.write(move |tx| claim(tx, &id, &assignees)).await
    }

    /// Take the user's logins `dropped` names off their issues where Shoal
    /// added them and no space links the issue for the same login any
    /// longer. Returns why each one that stays did.
    pub async fn release_assignees(
        &self,
        daemon: &dyn Daemon,
        dropped: Vec<Dropped>,
    ) -> Vec<String> {
        let mut warnings = Vec::new();
        for dropped in dropped {
            if let Err(error) = self.release_assignee(daemon, &dropped).await {
                let Dropped { url, login } = dropped;
                warnings.push(format!(
                    "{url} keeps its assignee {login}, since unassigning it on the forge failed: {error:#}; \
                     shoal issue edit {url} --remove-assignee {login} takes it off"
                ));
            }
        }
        warnings
    }

    /// [`Self::release_assignees`] for one login. Shoal forgets its
    /// assignment before asking the forge, so a failure leaves the assignee
    /// to the people who see it.
    async fn release_assignee(
        &self,
        daemon: &dyn Daemon,
        Dropped { url, login }: &Dropped,
    ) -> Result<()> {
        let _gate = self.gates.lock_issue_assignees(url).await;
        let (issue, assignee) = (url.clone(), login.clone());
        let taken = (self.store)
            .write(move |tx| take_assignment(tx, &issue, &assignee))
            .await?;
        if !taken {
            return Ok(());
        }
        let url: ItemRef = url.parse()?;
        let forge = daemon.item_forge(&url).await?;
        let removal = forge.unassign(&self.user_account(), &url, login).await?;
        self.record_assignee_change(&url, removal.edited).await;
        Ok(())
    }

    /// Record `edited`, the change Shoal just made on its own to the
    /// assignees of the issue `url`, as every space's own change, so no
    /// watch reports it. The caller holds the issue's assignee gate. A
    /// failure only lets watches report it, so it is logged.
    pub async fn record_assignee_change(&self, url: &ItemRef, edited: Edited) {
        if edited.is_empty() {
            return;
        }
        let issue = url.clone();
        let recorded = (self.store)
            .write(move |tx| record_shoal_change(tx, issue.url(), &edited))
            .await;
        if let Err(error) = recorded {
            log!("Shoal's assignee change of {url} not recorded: {error:#}");
        }
    }
}

/// Why the issue `url` was linked but not assigned on the forge.
pub fn unassigned(url: &ItemRef, error: &anyhow::Error) -> String {
    format!("{url} is linked, but assigning it on the forge failed: {error:#}")
}

/// What the caller hears of the issue `url`'s assignees but the accounts
/// Shoal assigns: the `others`.
pub fn also_assigned(url: &ItemRef, others: &[String]) -> String {
    let others = others.join(", ");
    format!("{url} is also assigned to {others}")
}

/// Record that the space `id` stands for the user's `login` among the
/// assignees of the issue `url`, and with `added` that Shoal added it. A
/// space that no longer links the issue gives the login back to release.
pub fn record_assignee(
    db: &Connection,
    id: &SpaceId,
    url: &str,
    login: &str,
    added: bool,
) -> Result<Option<Dropped>> {
    if added {
        db.execute(
            "INSERT OR IGNORE INTO issue_assignments(url,login) VALUES (?1,?2)",
            rusqlite::params![url, login],
        )?;
    }
    let linked = db.execute(
        "UPDATE space_issue SET assignee=?3 WHERE space_id=?1 AND url=?2",
        rusqlite::params![id, url, login],
    )? == 1;
    Ok((!linked).then(|| Dropped {
        url: url.to_owned(),
        login: login.to_owned(),
    }))
}

/// The logins the space `id` stands for among the assignees of the issues
/// `urls` it links as assigned, which it is about to drop.
pub fn dropped(db: &Connection, id: &SpaceId, urls: &[ItemRef]) -> Result<Vec<Dropped>> {
    let mut statement = db.prepare(
        "SELECT assignee FROM space_issue WHERE space_id=?1 AND url=?2 AND assignee IS NOT NULL",
    )?;
    let mut dropped = Vec::new();
    for url in urls {
        let login: Option<String> =
            (statement.query_row(rusqlite::params![id, url.url()], |row| row.get(0))).optional()?;
        dropped.extend(login.map(|login| Dropped {
            url: url.url().to_owned(),
            login,
        }));
    }
    Ok(dropped)
}

/// Record as Shoal's the user's assignees of the space `id` that
/// `assignees` says Shoal added, as a restored or moved space carries them,
/// for the issues the space links with an assignee again.
pub fn claim(db: &Connection, id: &SpaceId, assignees: &[IssueAssignee]) -> Result<()> {
    let added =
        (assignees.iter()).filter(|assignee| assignee.added && assignee.account == Assigner::User);
    for assignee in added {
        db.execute(
            "INSERT OR IGNORE INTO issue_assignments(url,login)
             SELECT url,assignee FROM space_issue
             WHERE space_id=?1 AND url=?2 AND assignee IS NOT NULL",
            rusqlite::params![id, assignee.url.url()],
        )?;
    }
    Ok(())
}

/// Hand over the user's assignees within the caller's transaction: the
/// space's links no longer say who assigned them, so its agent works none
/// here, and the agent account stays on the issues no other space here
/// links.
pub fn hand_over(db: &Connection, id: &SpaceId) -> Result<()> {
    db.execute(
        "UPDATE space_issue SET linked_by=NULL WHERE space_id=?1",
        [id],
    )?;
    let issues = crate::linked::assigned_urls(db, id)?;
    super::agents::hand_over_agent(db, &issues)?;
    for Dropped { url, login } in dropped(db, id, &issues)? {
        db.execute(
            "UPDATE space_issue SET assignee=NULL WHERE space_id=?1 AND url=?2",
            rusqlite::params![id, url],
        )?;
        take_assignment(db, &url, &login)?;
    }
    Ok(())
}

/// Forget Shoal's assignment of `login` to the issue `url` once no space
/// links the issue for that login, returning whether Shoal had made it.
pub fn take_assignment(db: &Connection, url: &str, login: &str) -> Result<bool> {
    let linked: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM space_issue WHERE url=?1 AND assignee=?2 COLLATE NOCASE)",
        rusqlite::params![url, login],
        |row| row.get(0),
    )?;
    if linked {
        return Ok(false);
    }
    let taken = db.execute(
        "DELETE FROM issue_assignments WHERE url=?1 AND login=?2",
        rusqlite::params![url, login],
    )?;
    Ok(taken == 1)
}
