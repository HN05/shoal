//! The agent account among the forge assignees of the issues spaces link as
//! assigned: it joins an issue while an agent works it and leaves it once
//! none does. Only Shoal assigns it, so Shoal reads the forge for it instead
//! of recording what it added.
use anyhow::Result;
use rusqlite::{Connection, params};
use shoal_ids::space_ids::SpaceId;

use crate::{
    client::space_names::SpaceSelector,
    daemon::{links::space::LinkSpace, workspace::Manager},
    execution::recovery,
    forge::items::reference::ItemRef,
    model::Assigner,
    state::QueueKind,
};
use shoal_forge::items::assignment::{Assignment, AssignmentRequests};

/// A space that links an issue as assigned, and who assigned it there.
struct Linker {
    id: SpaceId,
    assigner: Assigner,
}

/// What the store records of a space's agent.
struct AgentRecord {
    /// Whether the space's work is done.
    done: bool,
    /// Whether it has an agent that runs, is queued, restarts or waits for
    /// a conversion to restart it.
    present: bool,
}

impl Manager {
    /// Settle the agent account on each of `issues`, which `space` links as
    /// assigned or just dropped, where the space's `[issue] assign` assigns
    /// it. Returns why each one that could not be settled was not.
    pub(crate) async fn settle_issue_agents(
        &self,
        space: &LinkSpace,
        issues: &[ItemRef],
    ) -> Vec<String> {
        match self.space_settings(space).await {
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
    /// caller hears: with `others`, the issue's other assignees, or why it
    /// was not settled.
    pub(super) async fn assign_linked_agent(&self, url: &ItemRef, others: bool) -> Vec<String> {
        match self.settle_issue_agent(url).await {
            Ok(Some(assignment)) if others && !assignment.others.is_empty() => {
                vec![super::also_assigned(url, &assignment.others)]
            }
            Ok(_) => Vec::new(),
            Err(error) => vec![unsettled(url, &error)],
        }
    }

    /// Put the agent account on the issue `url` while an agent works it,
    /// and take it off once none does. Returns the assignment while one
    /// works it.
    async fn settle_issue_agent(&self, url: &ItemRef) -> Result<Option<Assignment>> {
        let _gate = self.gates.lock_issue_assignees(url.url()).await;
        let forge = self.item_forge(url).await?;
        let account = self.agent_account();
        match self.agent_works_issue(url).await? {
            true => forge.assign_own(&account, url).await,
            false => forge.unassign_own(&account, url).await.map(|_| None),
        }
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
    /// assigned it the issue and no agent of its own stopped.
    async fn agent_works(&self, linker: &Linker) -> Result<bool> {
        let space = self.link_space(&SpaceSelector::id(&linker.id)).await?;
        if !self.space_settings(&space).await?.issue.assign.agent() {
            return Ok(false);
        }
        let id = linker.id.clone();
        let record = self.store.read(move |tx| agent_record(tx, &id)).await?;
        if record.done {
            return Ok(false);
        }
        Ok(record.present
            || self.agent_sleeps(&linker.id)?
            || (linker.assigner == Assigner::Agent && !self.has_saved_agent(&linker.id)))
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

/// The issues the space `id` links as assigned whose agent account it
/// settles here: all but those it handed to the machine it moved to.
pub(super) fn settled_here(db: &Connection, id: &SpaceId) -> Result<Vec<ItemRef>> {
    Ok(db
        .prepare(
            "SELECT url FROM space_issue WHERE space_id=?1 AND linked_by IS NOT NULL
             ORDER BY rowid",
        )?
        .query_map([id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// The spaces here that link the issue `url` as assigned.
fn linkers(db: &Connection, url: &str) -> Result<Vec<Linker>> {
    Ok(db
        .prepare(
            "SELECT space_id,linked_by FROM space_issue WHERE url=?1 AND linked_by IS NOT NULL",
        )?
        .query_map([url], |row| {
            Ok(Linker {
                id: row.get(0)?,
                assigner: row.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// What the store records of the agent of the space `id`.
fn agent_record(db: &Connection, id: &SpaceId) -> Result<AgentRecord> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM space_completion WHERE space_id=?1),
            EXISTS(SELECT 1 FROM executions WHERE space_id=?1 AND agent IS NOT NULL)
            OR EXISTS(SELECT 1 FROM agent_queue WHERE space_id=?1 AND kind IN (?2,?3))
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
