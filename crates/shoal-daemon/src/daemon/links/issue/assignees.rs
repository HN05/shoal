//! The forge assignees of the issues spaces link as assigned: the account a
//! space acts as joins them when it links an issue, and leaves them once no
//! space of that login links the issue, if Shoal added it.
use crate::client::space_ids::SpaceId;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use shoal_states::states;

use crate::{
    daemon::{links::space::LinkSpace, scope::Caller, workspace::Manager},
    forge::{
        ForgeRepo,
        account::Account,
        items::{assignment::Assignment, reference::ItemRef},
    },
};

states!(
    /// Whose account assigns an issue: the user's own, for the user's
    /// terminal, or the agent account, for a scoped caller.
    Assigner {
        User => "user",
        Agent => "agent",
    }
);

impl Assigner {
    /// The account `caller` assigns as.
    pub(crate) fn of(caller: Option<&Caller>) -> Self {
        match caller {
            Some(_) => Self::Agent,
            None => Self::User,
        }
    }
}

/// The login a space stood for among an issue's assignees, until it
/// dropped the issue's link.
#[derive(Debug, Clone)]
pub(crate) struct Dropped {
    url: String,
    login: String,
}

impl Manager {
    /// Assign the issue `url`, of the repository of `forge`, which `space`
    /// just linked as assigned, to the account of `assigner`, where its
    /// `[issue] assign` says to. Returns what the caller hears: the issue's
    /// other assignees, or why it was not assigned.
    pub(super) async fn assign_on_forge(
        &self,
        space: &LinkSpace,
        forge: &ForgeRepo,
        url: &ItemRef,
        assigner: Assigner,
    ) -> Vec<String> {
        match self.space_settings(space).await {
            Ok(settings) if !settings.issue.assign => return Vec::new(),
            Ok(_) => {}
            Err(error) => return vec![unassigned(url, &error)],
        }
        match (self.assign_recorded(space.id(), forge, url, assigner)).await {
            Ok(Some(assignment)) if !assignment.others.is_empty() => {
                let others = assignment.others.join(", ");
                vec![format!("{url} is also assigned to {others}")]
            }
            Ok(_) => Vec::new(),
            Err(error) => vec![unassigned(url, &error)],
        }
    }

    /// Assign the issue `url` on `forge` as `assigner`, and record the login
    /// for the space `id` and, once Shoal added it, as Shoal's. A space that
    /// dropped the link meanwhile releases the login again.
    async fn assign_recorded(
        &self,
        id: &SpaceId,
        forge: &ForgeRepo,
        url: &ItemRef,
        assigner: Assigner,
    ) -> Result<Option<Assignment>> {
        let gate = self.gates.lock_issue_assignees(url.url()).await;
        let assigned = forge.assign_own(&self.account_of(assigner), url).await?;
        let Some(assignment) = assigned else {
            return Ok(None);
        };
        let (id, issue, login) = (id.to_owned(), url.clone(), assignment.login.clone());
        let added = assignment.added.then_some(assigner);
        let dropped = (self.store)
            .write(move |tx| record_assignee(tx, &id, issue.url(), &login, added))
            .await?;
        if let Some(dropped) = dropped {
            drop(gate);
            for warning in self.release_assignees(vec![dropped]).await {
                crate::daemon::log!("{warning}");
            }
        }
        Ok(Some(assignment))
    }

    /// Take the logins `dropped` names off their issues where Shoal added
    /// them and no space links the issue for the same login any longer.
    /// Returns why each one that stays did.
    pub(crate) async fn release_assignees(&self, dropped: Vec<Dropped>) -> Vec<String> {
        let mut warnings = Vec::new();
        for dropped in dropped {
            if let Err(error) = self.release_assignee(&dropped).await {
                let Dropped { url, login } = dropped;
                warnings.push(format!(
                    "{url} keeps its assignee {login}, since unassigning it on the forge failed: {error:#}"
                ));
            }
        }
        warnings
    }

    /// [`Self::release_assignees`] for one login, as the account that
    /// assigned it. Shoal forgets its assignment before asking the forge,
    /// so a failure leaves the assignee to the people who see it.
    async fn release_assignee(&self, Dropped { url, login }: &Dropped) -> Result<()> {
        let _gate = self.gates.lock_issue_assignees(url).await;
        let (issue, assignee) = (url.clone(), login.clone());
        let taken = (self.store)
            .write(move |tx| take_assignment(tx, &issue, &assignee))
            .await?;
        let Some(assigner) = taken else {
            return Ok(());
        };
        let url: ItemRef = url.parse()?;
        let forge = self.item_forge(&url).await?;
        forge
            .unassign(&self.account_of(assigner), &url, login)
            .await?;
        Ok(())
    }

    fn account_of(&self, assigner: Assigner) -> Account {
        match assigner {
            Assigner::User => self.user_account(),
            Assigner::Agent => self.agent_account(),
        }
    }
}

/// Why the issue `url` was linked but not assigned on the forge.
fn unassigned(url: &ItemRef, error: &anyhow::Error) -> String {
    format!("{url} is linked, but assigning it on the forge failed: {error:#}")
}

/// Record that the space `id` stands for `login` among the assignees of the
/// issue `url`, and with `added` that Shoal added it as that account. A
/// space that no longer links the issue gives the login back to release.
fn record_assignee(
    db: &Connection,
    id: &SpaceId,
    url: &str,
    login: &str,
    added: Option<Assigner>,
) -> Result<Option<Dropped>> {
    if let Some(account) = added {
        db.execute(
            "INSERT OR IGNORE INTO issue_assignments(url,login,account) VALUES (?1,?2,?3)",
            rusqlite::params![url, login, account],
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
pub(super) fn dropped(db: &Connection, id: &SpaceId, urls: &[ItemRef]) -> Result<Vec<Dropped>> {
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

/// Forget Shoal's assignment of `login` to the issue `url` once no space
/// links the issue for that login, returning the account that made it.
fn take_assignment(db: &Connection, url: &str, login: &str) -> Result<Option<Assigner>> {
    let linked: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM space_issue WHERE url=?1 AND assignee=?2 COLLATE NOCASE)",
        rusqlite::params![url, login],
        |row| row.get(0),
    )?;
    if linked {
        return Ok(None);
    }
    Ok(db
        .query_row(
            "DELETE FROM issue_assignments WHERE url=?1 AND login=?2 RETURNING account",
            rusqlite::params![url, login],
            |row| row.get(0),
        )
        .optional()?)
}

#[cfg(test)]
mod tests;
