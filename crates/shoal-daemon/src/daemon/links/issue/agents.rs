//! The agent account among the forge assignees of the issues spaces link as
//! assigned: it joins an issue while an agent works it and leaves it once
//! none does. Every Shoal acting as the account shares that assignee, so
//! each records the issues it put or kept the account on and takes it off
//! only those, and reads the forge for whether it is there.
use anyhow::Result;
use rusqlite::{Connection, params};
use shoal_ids::space_ids::SpaceId;
use shoal_links::issue::agents::{
    Linker, agent_assigned_here, forget_agent_assigned, linkers, record_agent_assigned,
    settled_here,
};
use shoal_links::space::LinkSpace;

use crate::{
    daemon::{manager::Manager, notifications::AlertKind},
    execution::recovery,
    forge::items::reference::ItemRef,
    model::{Assigner, Space},
    state::QueueKind,
};
use shoal_forge::items::assignment::AssignmentRequests;
use shoal_protocol::space_names::SpaceSelector;

/// What the store records of a space's agent.
struct AgentRecord {
    /// Whether the space's work is done.
    done: bool,
    /// Whether it has an agent that runs, is queued, restarts or waits for
    /// a conversion to restart it.
    present: bool,
}

impl Manager {
    /// Settle the agent account on the issues the space `id` links as
    /// assigned, as its agent starts or stops or its work is done or
    /// resumes. Each failure notifies.
    pub(crate) async fn settle_agent_assignees(&self, id: &SpaceId) {
        let (space, warnings) = match self.settle_space_agents(id).await {
            Ok(Some(settled)) => settled,
            Ok(None) => return,
            Err(error) => {
                crate::daemon::log!("agent assignees of {id} not settled: {error:#}");
                return;
            }
        };
        for warning in warnings {
            let kind = AlertKind::UnassignFailed;
            (self.notify_space(&space.name, id, kind, warning)).await;
        }
    }

    /// Settle the agent account on the issues the space `id` links as
    /// assigned. Returns the space with why each issue that could not be
    /// settled was not, or nothing when it links none, as a chat without a
    /// repository never does.
    async fn settle_space_agents(&self, id: &SpaceId) -> Result<Option<(LinkSpace, Vec<String>)>> {
        let owner = id.to_owned();
        let issues = (self.store)
            .read(move |tx| settled_here(tx, &owner))
            .await?;
        if issues.is_empty() {
            return Ok(None);
        }
        let space = self.links.link_space(self, &SpaceSelector::id(id)).await?;
        let warnings = self.settle_issue_agents(&space, &issues).await;
        Ok(Some((space, warnings)))
    }

    /// Settle the agent account on `issues`, which the removed `space`
    /// linked as assigned. Returns why each one that could not be settled
    /// was not; a space that linked none settles nothing.
    pub(crate) async fn settle_removed_agents(
        &self,
        space: &Space,
        issues: &[ItemRef],
    ) -> Result<Vec<String>> {
        if issues.is_empty() {
            return Ok(Vec::new());
        }
        let linked = self.links.own_link_space(self, space.clone()).await?;
        Ok(self.settle_issue_agents(&linked, issues).await)
    }

    /// Settle the agent account on each of `issues`, which `space` links as
    /// assigned or just dropped, where the space's `[issue] assign` assigns
    /// it. Returns why each one that could not be settled was not.
    pub(crate) async fn settle_issue_agents(
        &self,
        space: &LinkSpace,
        issues: &[ItemRef],
    ) -> Vec<String> {
        match self.acting_settings(space).await {
            Ok(settings) if !settings.issue.assign.agent() => return Vec::new(),
            Ok(_) => {}
            Err(error) => return issues.iter().map(|url| unsettled(url, &error)).collect(),
        }
        let mut warnings = Vec::new();
        for url in issues {
            if let Err(error) = self.settle_issue_agent(url).await {
                warnings.push(unsettled(url, &error));
            }
        }
        warnings
    }

    /// Settle the agent account on the issue `url`, which a space whose
    /// `[issue] assign` assigns it just linked as assigned. Returns what the
    /// caller hears: with `others`, the issue's assignees other than the
    /// agent account and the user, or why it was not settled.
    pub(super) async fn assign_linked_agent(&self, url: &ItemRef, others: bool) -> Vec<String> {
        let assigned = match self.settle_issue_agent(url).await {
            Ok(Some(assigned)) if others => self.besides_user(url, assigned).await,
            Ok(_) => Vec::new(),
            Err(error) => return vec![unsettled(url, &error)],
        };
        match assigned.is_empty() {
            true => Vec::new(),
            false => vec![shoal_links::issue::assignees::also_assigned(url, &assigned)],
        }
    }

    /// The `logins` of the issue `url`'s assignees but the user's own,
    /// which a user who assigned the issue to themselves needs no word of;
    /// all of them when the forge does not say which is the user's.
    async fn besides_user(&self, url: &ItemRef, logins: Vec<String>) -> Vec<String> {
        let user = match self.item_forge(url).await {
            Ok(forge) => forge.assignee_login(&self.links.user_account()).await,
            Err(error) => Err(error),
        };
        match user {
            Ok(Some(user)) => (logins.into_iter())
                .filter(|login| !login.eq_ignore_ascii_case(&user))
                .collect(),
            _ => logins,
        }
    }

    /// Put the agent account on the issue `url` while an agent works it,
    /// and take it off once none does. Returns the issue's other assignees,
    /// unless the agent account has no login of its own there.
    async fn settle_issue_agent(&self, url: &ItemRef) -> Result<Option<Vec<String>>> {
        let _gate = self.gates.lock_issue_assignees(url.url()).await;
        match self.agent_works_issue(url).await? {
            true => self.keep_agent_on(url).await,
            false => self.take_agent_off(url).await,
        }
    }

    /// Put the agent account on the issue `url`, or keep it there, and
    /// record that this Shoal did.
    async fn keep_agent_on(&self, url: &ItemRef) -> Result<Option<Vec<String>>> {
        let forge = self.item_forge(url).await?;
        let Some(assigned) = forge.assign_own(&self.links.agent_account(), url).await? else {
            return Ok(None);
        };
        self.links
            .record_assignee_change(url, assigned.edited)
            .await;
        let issue = url.url().to_owned();
        (self
            .store
            .write(move |tx| record_agent_assigned(tx, &issue)))
        .await?;
        Ok(Some(assigned.others))
    }

    /// Take the agent account off the issue `url` where this Shoal put or
    /// kept it there, and leave it where another Shoal acting as the same
    /// account may have, whose agent may still work the issue.
    async fn take_agent_off(&self, url: &ItemRef) -> Result<Option<Vec<String>>> {
        let forge = self.item_forge(url).await?;
        let account = self.links.agent_account();
        let issue = url.url().to_owned();
        if !(self.store.read(move |tx| agent_assigned_here(tx, &issue))).await? {
            return forge.others_assigned(&account, url).await;
        }
        let removed = forge.unassign_own(&account, url).await?;
        if let Some(removed) = &removed {
            (self
                .links
                .record_assignee_change(url, removed.edited.clone()))
            .await;
        }
        let issue = url.url().to_owned();
        (self
            .store
            .write(move |tx| forget_agent_assigned(tx, &issue)))
        .await?;
        Ok(removed.map(|removed| removed.others))
    }

    /// Whether an agent works the issue `url`: that of a space linking it
    /// as assigned.
    async fn agent_works_issue(&self, url: &ItemRef) -> Result<bool> {
        let issue = url.url().to_owned();
        let linkers = self.store.read(move |tx| linkers(tx, &issue)).await?;
        for linker in linkers {
            if self.agent_works(&linker).await? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Whether the agent of `linker`'s space works the issue it links,
    /// where its `[issue] assign` assigns the agent account: while its
    /// work is not done, it has an agent or one asleep, or else an agent
    /// assigned it the issue and no agent of its own ended since.
    async fn agent_works(&self, linker: &Linker) -> Result<bool> {
        let space = self
            .links
            .link_space(self, &SpaceSelector::id(&linker.id))
            .await?;
        if !self.acting_settings(&space).await?.issue.assign.agent() {
            return Ok(false);
        }
        let id = linker.id.clone();
        let record = self.store.read(move |tx| agent_record(tx, &id)).await?;
        if record.done {
            return Ok(false);
        }
        Ok(record.present || self.agent_sleeps(&linker.id)? || linker.assigner == Assigner::Agent)
    }

    /// Whether the agent of the space `id` is asleep, stopped by Shoal with
    /// a restore requested that the agent queue has yet to take.
    fn agent_sleeps(&self, id: &SpaceId) -> Result<bool> {
        Ok(!recovery::restore_requests(&self.paths, id)?.is_empty())
    }
}

/// Why the agent account could not be settled on the issue `url`.
fn unsettled(url: &ItemRef, error: &anyhow::Error) -> String {
    format!(
        "the agent account's assignment of {url} was not settled on the forge: {error:#}; \
         shoal issue edit {url} --add-assignee or --remove-assignee settles it"
    )
}

/// What the store records of the agent of the space `id`.
fn agent_record(db: &Connection, id: &SpaceId) -> Result<AgentRecord> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM space_completion WHERE space_id=?1),
            EXISTS(SELECT 1 FROM executions WHERE space_id=?1 AND agent IS NOT NULL)
            OR EXISTS(SELECT 1 FROM agent_queue WHERE kind IN (?2,?3)
                AND ?1 IN (space_id,opens_workspace_id))
            OR EXISTS(SELECT 1 FROM space_conversions WHERE space_id=?1)",
        params![id, QueueKind::Launch, QueueKind::Restore],
        |row| {
            Ok(AgentRecord {
                done: row.get(0)?,
                present: row.get(1)?,
            })
        },
    )?)
}

#[cfg(test)]
mod tests;
