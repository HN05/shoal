//! The user's own login among the forge assignees of the issues spaces link
//! as assigned: it joins them on each link where `[issue] assign` says so,
//! whoever links the issue, and leaves them once no space links the issue
//! for it, if Shoal added it.
use anyhow::Result;
use shoal_ids::space_ids::SpaceId;
use shoal_links::issue::assignees::{Dropped, claim, hand_over, record_assignee, take_assignment};

use crate::{
    daemon::{
        links::wait::record_shoal_change, manager::Manager, notifications::AlertKind, scope::Caller,
    },
    forge::{ForgeRepo, items::reference::ItemRef},
    model::{Assigner, IssueAssignee},
};
use shoal_forge::{
    items::assignment::{Assignment, AssignmentRequests},
    updates::Edited,
};

/// Who a caller assigns an issue to a space as.
pub(crate) trait AssignerOf {
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

impl Manager {
    /// Assign the issue `url`, of the repository of `forge`, which the
    /// space `id` just linked as assigned, to the user's own login. Returns
    /// what the caller hears: the issue's other assignees, or why it was
    /// not assigned.
    pub(super) async fn assign_on_forge(
        &self,
        id: &SpaceId,
        forge: &ForgeRepo,
        url: &ItemRef,
    ) -> Vec<String> {
        match (self.assign_recorded(id, forge, url)).await {
            Ok(Some(assignment)) if !assignment.others.is_empty() => {
                vec![super::also_assigned(url, &assignment.others)]
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
        id: &SpaceId,
        forge: &ForgeRepo,
        url: &ItemRef,
    ) -> Result<Option<Assignment>> {
        let gate = self.gates.lock_issue_assignees(url.url()).await;
        let assigned = forge.assign_own(&self.links.user_account(), url).await?;
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
            for warning in self.release_assignees(vec![dropped]).await {
                self.notify(None, AlertKind::UnassignFailed, warning).await;
            }
        }
        Ok(Some(assignment))
    }

    /// Hand the assignees the space `id` stands for to the machine it moved
    /// to: forget them for it, and Shoal's assignments no other space here
    /// links, so ending it here takes none off the forge.
    pub(crate) async fn hand_over_assignees(&self, id: &SpaceId) -> Result<()> {
        let id = id.to_owned();
        self.store.write(move |tx| hand_over(tx, &id)).await
    }

    /// Take on the user's assignments `assignees` says Shoal added, which a
    /// restored or moved space `id` carries, so its removal takes them back.
    pub(crate) async fn claim_assignees(
        &self,
        id: &SpaceId,
        assignees: Vec<IssueAssignee>,
    ) -> Result<()> {
        let id = id.to_owned();
        self.store.write(move |tx| claim(tx, &id, &assignees)).await
    }

    /// Take the user's logins `dropped` names off their issues where Shoal
    /// added them and no space links the issue for the same login any
    /// longer. Returns why each one that stays did.
    pub(crate) async fn release_assignees(&self, dropped: Vec<Dropped>) -> Vec<String> {
        let mut warnings = Vec::new();
        for dropped in dropped {
            if let Err(error) = self.release_assignee(&dropped).await {
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
    async fn release_assignee(&self, Dropped { url, login }: &Dropped) -> Result<()> {
        let _gate = self.gates.lock_issue_assignees(url).await;
        let (issue, assignee) = (url.clone(), login.clone());
        let taken = (self.store)
            .write(move |tx| take_assignment(tx, &issue, &assignee))
            .await?;
        if !taken {
            return Ok(());
        }
        let url: ItemRef = url.parse()?;
        let forge = self.item_forge(&url).await?;
        let removal = forge
            .unassign(&self.links.user_account(), &url, login)
            .await?;
        self.record_assignee_change(&url, removal.edited).await;
        Ok(())
    }

    /// Record `edited`, the change Shoal just made on its own to the
    /// assignees of the issue `url`, as every space's own change, so no
    /// watch reports it. The caller holds the issue's assignee gate. A
    /// failure only lets watches report it, so it is logged.
    pub(super) async fn record_assignee_change(&self, url: &ItemRef, edited: Edited) {
        if edited.is_empty() {
            return;
        }
        let issue = url.clone();
        let recorded = (self.store)
            .write(move |tx| record_shoal_change(tx, issue.url(), &edited))
            .await;
        if let Err(error) = recorded {
            crate::daemon::log!("Shoal's assignee change of {url} not recorded: {error:#}");
        }
    }
}

/// Why the issue `url` was linked but not assigned on the forge.
pub(super) fn unassigned(url: &ItemRef, error: &anyhow::Error) -> String {
    format!("{url} is linked, but assigning it on the forge failed: {error:#}")
}

#[cfg(test)]
mod tests;
