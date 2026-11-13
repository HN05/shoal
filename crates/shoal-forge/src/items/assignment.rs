//! Assigning an issue to the account that works on it, and taking that
//! assignment back.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{
    ForgeRepo,
    account::Account,
    backends::Backend,
    capability::Capability,
    items::{
        action::{Action, Edit},
        item::{Item, ItemState},
        link::ItemKind,
        reference::ItemRef,
    },
};

/// What assigning an issue to the acting account found and did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assignment {
    /// The login the account signs in as.
    pub login: String,
    /// Whether the assignment added the login: not when the issue had it
    /// already, nor when it is closed.
    pub added: bool,
    /// The issue's other assignees.
    pub others: Vec<String>,
}

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
        // Read as no other account, since the change goes out as this one.
        if !account.has_login(self) {
            return Ok(None);
        }
        self.require(Capability::IssueAssignees)?;
        let login = (self.kind.own_login(self, account).await?)
            .context("the forge did not report the account's login")?;
        let current = self.issue_details(account, number).await?.item;
        let assigned = has_assignee(&current, &login);
        let added = !assigned && current.state == ItemState::Open;
        if added {
            let edit = Edit {
                add_assignees: vec![login.clone()],
                ..Edit::default()
            };
            (self.act(account, ItemKind::Issue, number.get(), &Action::edit(edit))).await?;
        }
        let others = (current.assignees.into_iter())
            .filter(|assignee| !assignee.eq_ignore_ascii_case(&login))
            .collect();
        Ok(Some(Assignment {
            login,
            added,
            others,
        }))
    }

    /// Remove `login` from the assignees of `issue`, of this repository,
    /// returning whether it did: a closed issue keeps its assignees.
    pub async fn unassign(&self, account: &Account, issue: &ItemRef, login: &str) -> Result<bool> {
        issue.belongs_to(self)?;
        let number = issue.issue()?;
        let current = self.issue_details(account, number).await?.item;
        if current.state != ItemState::Open || !has_assignee(&current, login) {
            return Ok(false);
        }
        let edit = Edit {
            remove_assignees: vec![login.to_owned()],
            ..Edit::default()
        };
        (self.act(account, ItemKind::Issue, number.get(), &Action::edit(edit))).await?;
        Ok(true)
    }
}

/// Whether `login` is among the assignees of `item`; logins name the same
/// user whatever their case.
fn has_assignee(item: &Item, login: &str) -> bool {
    (item.assignees.iter()).any(|assignee| assignee.eq_ignore_ascii_case(login))
}

#[cfg(test)]
mod tests;
