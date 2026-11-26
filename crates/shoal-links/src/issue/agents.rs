//! The agent account among the assignees of the issues spaces link as
//! assigned. `docs/design/forge.md` says when it joins and leaves.
use anyhow::Result;
use rusqlite::{Connection, params};
use shoal_daemon_base::{
    alerts::{About, NewAlert},
    log,
};
use shoal_forge::items::assignment::AssignmentRequests;
use shoal_forge_types::items::reference::ItemRef;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    alerts::AlertKind,
    model::{Assigner, Space},
    space_names::SpaceSelector,
};

use crate::{Links, daemon::Daemon, space::LinkSpace};

/// A space that links an issue as assigned, and who assigned it there.
pub struct Linker {
    pub id: SpaceId,
    pub assigner: Assigner,
}

impl Links {
    /// Settle the agent account on the issues the space `id` links as
    /// assigned, as its agent starts or stops or its work is done or
    /// resumes. Each failure notifies.
    pub async fn settle_agent_assignees(&self, daemon: &dyn Daemon, id: &SpaceId) {
        let (space, warnings) = match self.settle_space_agents(daemon, id).await {
            Ok(Some(settled)) => settled,
            Ok(None) => return,
            Err(error) => {
                log!("agent assignees of {id} not settled: {error:#}");
                return;
            }
        };
        for warning in warnings {
            let about = About::space(&space.name, id);
            let alert = NewAlert::shoal(AlertKind::UnassignFailed, about, warning);
            self.notifications.notify(daemon, alert).await;
        }
    }

    /// Settle the agent account on the issues the space `id` links as
    /// assigned. Returns the space with why each issue that could not be
    /// settled was not, or nothing when it links none, as a chat without a
    /// repository never does.
    pub async fn settle_space_agents(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
    ) -> Result<Option<(LinkSpace, Vec<String>)>> {
        let owner = id.to_owned();
        let issues = (self.store)
            .read(move |tx| settled_here(tx, &owner))
            .await?;
        if issues.is_empty() {
            return Ok(None);
        }
        let space = self.link_space(daemon, &SpaceSelector::id(id)).await?;
        let warnings = self.settle_issue_agents(daemon, &space, &issues).await;
        Ok(Some((space, warnings)))
    }

    /// Settle the agent account on `issues`, which the removed `space`
    /// linked as assigned. Returns why each one that could not be settled
    /// was not; a space that linked none settles nothing.
    pub async fn settle_removed_agents(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        issues: &[ItemRef],
    ) -> Result<Vec<String>> {
        if issues.is_empty() {
            return Ok(Vec::new());
        }
        let linked = self.own_link_space(daemon, space.clone()).await?;
        Ok(self.settle_issue_agents(daemon, &linked, issues).await)
    }

    /// Settle the agent account on each of `issues`, which `space` links as
    /// assigned or just dropped, where the space's `[issue] assign` assigns
    /// it. Returns why each one that could not be settled was not.
    pub async fn settle_issue_agents(
        &self,
        daemon: &dyn Daemon,
        space: &LinkSpace,
        issues: &[ItemRef],
    ) -> Vec<String> {
        match daemon.acting_settings(space).await {
            Ok(settings) if !settings.issue.assign.agent() => return Vec::new(),
            Ok(_) => {}
            Err(error) => return issues.iter().map(|url| unsettled(url, &error)).collect(),
        }
        let mut warnings = Vec::new();
        for url in issues {
            if let Err(error) = self.settle_issue_agent(daemon, url).await {
                warnings.push(unsettled(url, &error));
            }
        }
        warnings
    }

    /// Settle the agent account on the issue `url`, which a space whose
    /// `[issue] assign` assigns it just linked as assigned. Returns what the
    /// caller hears: with `others`, the issue's assignees other than the
    /// agent account and the user, or why it was not settled.
    pub async fn assign_linked_agent(
        &self,
        daemon: &dyn Daemon,
        url: &ItemRef,
        others: bool,
    ) -> Vec<String> {
        let assigned = match self.settle_issue_agent(daemon, url).await {
            Ok(Some(assigned)) if others => self.besides_user(url, assigned).await,
            Ok(_) => Vec::new(),
            Err(error) => return vec![unsettled(url, &error)],
        };
        match assigned.is_empty() {
            true => Vec::new(),
            false => vec![super::assignees::also_assigned(url, &assigned)],
        }
    }

    /// The `logins` of the issue `url`'s assignees but the user's own,
    /// which a user who assigned the issue to themselves needs no word of;
    /// all of them when the forge does not say which is the user's.
    async fn besides_user(&self, url: &ItemRef, logins: Vec<String>) -> Vec<String> {
        let user = match self.forges.item_forge(url).await {
            Ok(forge) => forge.assignee_login(&self.forges.user_account()).await,
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
    async fn settle_issue_agent(
        &self,
        daemon: &dyn Daemon,
        url: &ItemRef,
    ) -> Result<Option<Vec<String>>> {
        let _gate = self.gates.lock_issue_assignees(url.url()).await;
        match self.agent_works_issue(daemon, url).await? {
            true => self.keep_agent_on(url).await,
            false => self.take_agent_off(url).await,
        }
    }

    /// Put the agent account on the issue `url`, or keep it there, and
    /// record that this Shoal did.
    async fn keep_agent_on(&self, url: &ItemRef) -> Result<Option<Vec<String>>> {
        let forge = self.forges.item_forge(url).await?;
        let Some(assigned) = forge.assign_own(&self.forges.agent_account(), url).await? else {
            return Ok(None);
        };
        self.record_assignee_change(url, assigned.edited).await;
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
        let forge = self.forges.item_forge(url).await?;
        let account = self.forges.agent_account();
        let issue = url.url().to_owned();
        if !(self.store.read(move |tx| agent_assigned_here(tx, &issue))).await? {
            return forge.others_assigned(&account, url).await;
        }
        let removed = forge.unassign_own(&account, url).await?;
        if let Some(removed) = &removed {
            (self.record_assignee_change(url, removed.edited.clone())).await;
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
    async fn agent_works_issue(&self, daemon: &dyn Daemon, url: &ItemRef) -> Result<bool> {
        let issue = url.url().to_owned();
        let linkers = self.store.read(move |tx| linkers(tx, &issue)).await?;
        for linker in linkers {
            if self.agent_works(daemon, &linker).await? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Whether the agent of `linker`'s space works the issue it links,
    /// where its `[issue] assign` assigns the agent account: while its
    /// work is not done, it has an agent or one asleep, or else an agent
    /// assigned it the issue and no agent of its own ended since.
    async fn agent_works(&self, daemon: &dyn Daemon, linker: &Linker) -> Result<bool> {
        let space = self
            .link_space(daemon, &SpaceSelector::id(&linker.id))
            .await?;
        if !daemon.acting_settings(&space).await?.issue.assign.agent() {
            return Ok(false);
        }
        let agent = daemon.space_agent(&linker.id).await?;
        if agent.done {
            return Ok(false);
        }
        Ok(agent.present || linker.assigner == Assigner::Agent)
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
pub fn settled_here(db: &Connection, id: &SpaceId) -> Result<Vec<ItemRef>> {
    Ok(db
        .prepare(
            "SELECT url FROM space_issue WHERE space_id=?1 AND linked_by IS NOT NULL
             ORDER BY rowid",
        )?
        .query_map([id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Record that an agent of the space `id` ended: the issues an agent assigned
/// it count as worked only while it has an agent again.
pub fn agent_ended(db: &Connection, id: &SpaceId) -> Result<()> {
    db.execute(
        "UPDATE space_issue SET linked_by=?2 WHERE space_id=?1 AND linked_by=?3",
        params![id, Assigner::User, Assigner::Agent],
    )?;
    Ok(())
}

/// Record that this Shoal put or kept the agent account on the issue `url`.
pub fn record_agent_assigned(db: &Connection, url: &str) -> Result<()> {
    db.execute(
        "INSERT OR IGNORE INTO agent_assigned_issues(url) VALUES (?1)",
        [url],
    )?;
    Ok(())
}

/// Whether this Shoal put or kept the agent account on the issue `url`.
pub fn agent_assigned_here(db: &Connection, url: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_assigned_issues WHERE url=?1)",
        [url],
        |row| row.get(0),
    )?)
}

/// Forget that this Shoal put the agent account on the issue `url`.
pub fn forget_agent_assigned(db: &Connection, url: &str) -> Result<()> {
    db.execute("DELETE FROM agent_assigned_issues WHERE url=?1", [url])?;
    Ok(())
}

/// Leave the agent account on `issues`, which a space that moved to
/// another machine links, to that machine, where no space here settles it.
pub fn hand_over_agent(db: &Connection, issues: &[ItemRef]) -> Result<()> {
    for url in issues {
        db.execute(
            "DELETE FROM agent_assigned_issues WHERE url=?1 AND NOT EXISTS(
                SELECT 1 FROM space_issue WHERE url=?1 AND linked_by IS NOT NULL)",
            [url.url()],
        )?;
    }
    Ok(())
}

/// The spaces here that link the issue `url` as assigned.
pub fn linkers(db: &Connection, url: &str) -> Result<Vec<Linker>> {
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
