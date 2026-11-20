//! Persist issue associations.
use shoal_ids::space_ids::SpaceId;
use shoal_links::space::LinkSpace;
use shoal_protocol::space_names::SpaceSelector;

use anyhow::Result;
use rusqlite::Connection;

pub(crate) mod agent_settles;
mod agents;
mod assignees;

pub(crate) use crate::model::{Assigner, IssueAssignee};
use crate::{
    daemon::manager::Manager,
    forge::{
        ForgeRepo,
        items::reference::{ItemInput, ItemRef},
    },
};
pub(crate) use assignees::AssignerOf;
use shoal_links::issue::{
    agents::{self as agent_records},
    assignees::{self as assignee_records, Dropped},
};

impl Manager {
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
        let id = self.links.link_space(self, selector).await?.id.to_owned();
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
        let space = self.links.link_space(self, selector).await?;
        self.verify_space(&space).await?;
        let (forge, url) = self
            .links
            .issue_forge(self, space.origin_path(), input)
            .await?;
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
        let assign = match self.acting_settings(space).await {
            Ok(settings) => settings.issue.assign,
            Err(error) => return vec![shoal_links::issue::assignees::unassigned(url, &error)],
        };
        let mut warnings = Vec::new();
        if fresh && assign.user() {
            warnings.extend(
                self.links
                    .assign_on_forge(self, &space.id, forge, url)
                    .await,
            );
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
        let (id, url, links) = (id.to_owned(), url.clone(), self.links.clone());
        self.store
            .write(move |tx| links.assign(tx, &id, url.url(), title.as_deref(), assigner))
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
        let id = self.links.link_space(self, selector).await?.id.to_owned();
        let _guard = self.gates.lock_prs([&id]).await;
        let space = self.links.link_space(self, &SpaceSelector::id(&id)).await?;
        let selected = match input {
            Some(input) => Some(
                self.links
                    .issue_forge(self, space.origin_path(), input)
                    .await?
                    .1,
            ),
            None => None,
        };
        let links = self.links.clone();
        let (removed, dropped) = self
            .store
            .write(move |tx| links.unlink(tx, &id, selected.as_ref().map(ItemRef::url)))
            .await?;
        self.signals.wake_cleanup();
        let mut warnings = self.links.release_assignees(self, dropped).await;
        warnings.extend(self.settle_issue_agents(&space, &removed).await);
        Ok(warnings)
    }
}

/// The user's assignees the space `id` stands for on every issue it links
/// as assigned, which its removal drops.
pub(crate) fn removed_assignees(db: &Connection, id: &SpaceId) -> Result<Vec<Dropped>> {
    assignee_records::dropped(db, id, &shoal_links::linked::assigned_urls(db, id)?)
}

/// The issues the space `id` links as assigned whose agent account its
/// removal settles: all but those it handed to the machine it moved to.
pub(crate) fn removed_issues(db: &Connection, id: &SpaceId) -> Result<Vec<ItemRef>> {
    agent_records::settled_here(db, id)
}

/// Record that an agent of the space `id` ended, within the caller's
/// transaction, so the issues an agent assigned it no longer count as
/// worked without one.
pub(crate) fn agent_ended(db: &Connection, id: &SpaceId) -> Result<()> {
    agent_records::agent_ended(db, id)
}

#[cfg(test)]
pub(in crate::daemon::links) mod test_support;
#[cfg(test)]
mod tests;
