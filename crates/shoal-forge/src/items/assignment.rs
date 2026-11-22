//! Assigning an issue to the account that works on it, and taking that
//! assignment back.
use anyhow::{Context, Result};

use crate::{
    RepoRequests,
    capability::CapabilityRequests,
    items::{Items, action::ActionRequests},
    transport::account::Account,
    updates::Edited,
};
use shoal_forge_types::{
    ForgeRepo,
    capability::Capability,
    items::{
        action::{Action, Edit},
        item::{Item, ItemState},
        link::ItemKind,
        reference::ItemRef,
    },
};

/// What assigning an issue to the acting account found and did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    /// The login the account signs in as, as assignees name it: Bitbucket's
    /// account ID.
    pub login: String,
    /// Whether the assignment added the login: not when the issue had it
    /// already, nor when it is closed.
    pub added: bool,
    /// The issue's other assignees.
    pub others: Vec<String>,
    /// What adding the login changed of the issue's details.
    pub edited: Edited,
}

/// What taking the acting account's login off an issue found and did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unassignment {
    /// Whether it took the login off: not when the issue lacked it, nor
    /// when it is closed.
    pub removed: bool,
    /// The issue's other assignees.
    pub others: Vec<String>,
    /// What taking the login off changed of the issue's details.
    pub edited: Edited,
}

/// Assigning items.
#[shoal_forge_derive::extension(pub AssignmentRequests)]
impl ForgeRepo {
    /// Add the login `account` signs in as to the assignees of `issue`, of
    /// this repository, unless it has the login already or is closed. An
    /// account without a login of its own there assigns nothing.
    pub async fn assign_own(
        &self,
        account: &Account,
        issue: &ItemRef,
    ) -> Result<Option<Assignment>> {
        issue.belongs_to(self)?;
        let number = issue.issue()?;
        let Some(login) = self.assigning_login(account).await? else {
            return Ok(None);
        };
        let current = self.issue_details(account, number).await?.item;
        let assigned = has_assignee(&current, &login);
        let added = !assigned && current.state == ItemState::Open;
        let edited = match added {
            true => {
                let edit = Edit {
                    add_assignees: vec![login.clone()],
                    ..Edit::default()
                };
                edit_assignees(self, account, issue, edit).await?
            }
            false => Edited::default(),
        };
        let others = besides(current.assignees, &login);
        Ok(Some(Assignment {
            login,
            added,
            others,
            edited,
        }))
    }

    /// How `account` is named among this repository's assignees: none for
    /// an account without a login of its own there.
    pub async fn assignee_login(&self, account: &Account) -> Result<Option<String>> {
        if !account.has_login(self) {
            return Ok(None);
        }
        self.kind().own_assignee(self, account).await
    }

    /// Remove the login `account` signs in as from the assignees of `issue`,
    /// of this repository, unless it is closed. An account without a login
    /// of its own there removes nothing.
    pub async fn unassign_own(
        &self,
        account: &Account,
        issue: &ItemRef,
    ) -> Result<Option<Unassignment>> {
        issue.belongs_to(self)?;
        let Some(login) = self.assigning_login(account).await? else {
            return Ok(None);
        };
        take_off(self, account, issue, &login).await.map(Some)
    }

    /// The assignees of `issue`, of this repository, besides the login
    /// `account` signs in as, read without changing them. An account
    /// without a login of its own there reads none.
    pub async fn others_assigned(
        &self,
        account: &Account,
        issue: &ItemRef,
    ) -> Result<Option<Vec<String>>> {
        issue.belongs_to(self)?;
        let Some(login) = self.assigning_login(account).await? else {
            return Ok(None);
        };
        let current = self.issue_details(account, issue.issue()?).await?.item;
        Ok(Some(besides(current.assignees, &login)))
    }

    /// The login `account` assigns and unassigns itself as in this
    /// repository: none for an account without a login of its own there,
    /// which reads as no other account, since its changes go out as this
    /// one.
    async fn assigning_login(&self, account: &Account) -> Result<Option<String>> {
        if !account.has_login(self) {
            return Ok(None);
        }
        self.require(Capability::IssueAssignees)?;
        let login = (self.kind().own_assignee(self, account).await?)
            .context("the forge did not report the account's login")?;
        Ok(Some(login))
    }

    /// Remove `login` from the assignees of `issue`, of this repository:
    /// a closed issue keeps its assignees.
    pub async fn unassign(
        &self,
        account: &Account,
        issue: &ItemRef,
        login: &str,
    ) -> Result<Unassignment> {
        issue.belongs_to(self)?;
        take_off(self, account, issue, login).await
    }
}

/// Remove `login` from the assignees of `issue`, of `repo`, as `account`,
/// unless the issue lacks it or is closed.
async fn take_off(
    repo: &ForgeRepo,
    account: &Account,
    issue: &ItemRef,
    login: &str,
) -> Result<Unassignment> {
    let number = issue.issue()?;
    let current = repo.issue_details(account, number).await?.item;
    let removed = current.state == ItemState::Open && has_assignee(&current, login);
    let edited = match removed {
        true => {
            let edit = Edit {
                remove_assignees: vec![login.to_owned()],
                ..Edit::default()
            };
            edit_assignees(repo, account, issue, edit).await?
        }
        false => Edited::default(),
    };
    let others = besides(current.assignees, login);
    Ok(Unassignment {
        removed,
        others,
        edited,
    })
}

/// Apply `edit` of the assignees of `issue`, of `repo`, as `account`,
/// returning what it changed.
async fn edit_assignees(
    repo: &ForgeRepo,
    account: &Account,
    issue: &ItemRef,
    edit: Edit,
) -> Result<Edited> {
    let number = issue.issue()?.get();
    let acted = (repo.apply(account, ItemKind::Issue, number, &Action::edit(edit))).await?;
    Ok(acted.edited)
}

/// `assignees` without `login`, whatever its case.
fn besides(assignees: Vec<String>, login: &str) -> Vec<String> {
    (assignees.into_iter())
        .filter(|assignee| !assignee.eq_ignore_ascii_case(login))
        .collect()
}

/// Whether `login` is among the assignees of `item`; logins name the same
/// user whatever their case.
fn has_assignee(item: &Item, login: &str) -> bool {
    (item.assignees.iter()).any(|assignee| assignee.eq_ignore_ascii_case(login))
}

#[cfg(test)]
mod tests;
