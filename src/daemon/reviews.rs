//! Local reviews of workspaces and PRs: queueing them, running each once in
//! a detached checkout, and recording what the reviewer found and how the
//! workspace's agent answered it.
use std::collections::HashMap;

use anyhow::{Context, Result, bail, ensure};
use tokio::sync::watch;

use super::{log, workspace::Manager};
use crate::{
    agent::Agent,
    config::Effective,
    model::Workspace,
    reviews::{FindingId, Outcome, Review, ReviewTarget},
};

mod history;
mod reviewer;
mod run;
pub(crate) mod store;

pub(crate) use run::run;

/// The daemon's running reviews.
pub(crate) struct Reviews {
    /// Wakes the scheduler when a review is queued.
    queued: tokio::sync::Notify,
    /// Stops each review that has started to run.
    cancels: std::sync::Mutex<HashMap<i64, watch::Sender<bool>>>,
    /// Counts the runs that ended, for waits on one.
    ended: watch::Sender<u64>,
}

impl Default for Reviews {
    fn default() -> Self {
        Self {
            queued: Default::default(),
            cancels: Default::default(),
            ended: watch::channel(0).0,
        }
    }
}

impl Manager {
    /// Queue a review of the workspace by `agent`, else `[review] agent`, else
    /// `agent.default`, or return the one already queued. A running review
    /// refuses another.
    pub async fn queue_review(
        &self,
        selector: &str,
        target: ReviewTarget,
        agent: Option<Agent>,
    ) -> Result<Review> {
        let workspace = self.workspace(selector).await?;
        let settings = self.workspace_settings(&workspace).await?;
        let agent = reviewing_agent(&settings, agent)?;
        let (id, now) = (workspace.id.clone(), now());
        let queued = self
            .store
            .run(move |db| store::queue(db, &id, &target, &agent, now))
            .await?;
        self.reviews.queued.notify_one();
        self.review(queued.id()).await
    }

    /// Cancel review `id`, else the workspace's queued or running one, for a
    /// process of workspace `by` when one asked.
    pub async fn cancel_review(
        &self,
        selector: Option<&str>,
        id: Option<i64>,
        by: Option<String>,
    ) -> Result<Review> {
        let id = match (id, selector) {
            (Some(id), _) => id,
            (None, Some(selector)) => self.active_review(selector).await?,
            (None, None) => bail!("name a review or a workspace"),
        };
        let now = now();
        let cancelled = self
            .store
            .run(move |db| store::cancel(db, id, by.as_deref(), now))
            .await?;
        ensure!(cancelled, "review {id} has already ended");
        if let Some(cancel) = self.reviews.cancels.lock().unwrap().get(&id) {
            cancel.send_replace(true);
        }
        self.review(id).await
    }

    /// The ID of the workspace's queued or running review.
    async fn active_review(&self, selector: &str) -> Result<i64> {
        let workspace = self.workspace(selector).await?;
        let id = workspace.id.clone();
        let active = self.store.run(move |db| store::active(db, &id)).await?;
        let (id, _) = active
            .with_context(|| format!("{} has no queued or running review", workspace.name))?;
        Ok(id)
    }

    /// Review `id` in full, marked outdated once its workspace's HEAD moved.
    pub async fn review(&self, id: i64) -> Result<Review> {
        let review = self.store.run(move |db| store::load(db, id)).await?;
        let mut review = review.with_context(|| format!("review {id} does not exist"))?;
        self.annotate_reviews(std::slice::from_mut(&mut review))
            .await;
        Ok(review)
    }

    /// The workspace's newest review.
    pub async fn latest_review(&self, selector: &str) -> Result<Review> {
        let workspace = self.workspace(selector).await?;
        let id = workspace.id.clone();
        let latest = self.store.run(move |db| store::latest(db, &id)).await?;
        let mut review = latest.with_context(|| {
            format!(
                "{} has no reviews; start one with shoal review run",
                workspace.name
            )
        })?;
        self.annotate_reviews(std::slice::from_mut(&mut review))
            .await;
        Ok(review)
    }

    /// The workspaces whose reviews a listing shows: the one named, else the
    /// repository's, else every workspace.
    pub async fn review_workspaces(
        &self,
        workspace: Option<&str>,
        repository: Option<&str>,
    ) -> Result<Vec<String>> {
        if let Some(workspace) = workspace {
            return Ok(vec![self.workspace(workspace).await?.id]);
        }
        let repository = match repository {
            Some(repository) => Some(self.repository(repository).await?.id),
            None => None,
        };
        self.store
            .run(move |db| store::workspaces(db, repository.as_deref()))
            .await
    }

    /// The reviews of the workspaces `ids`, newest first.
    pub async fn reviews(&self, ids: Vec<String>) -> Result<Vec<Review>> {
        let mut reviews = self.store.run(move |db| store::list(db, &ids)).await?;
        self.annotate_reviews(&mut reviews).await;
        Ok(reviews)
    }

    /// Review `id` once it is neither queued nor running.
    pub async fn await_review(&self, id: i64) -> Result<Review> {
        let mut changed = self.store.watch_changes();
        let mut ended = self.reviews.ended.subscribe();
        loop {
            let review = self.review(id).await?;
            if !review.state.is_active() {
                return Ok(review);
            }
            tokio::select! {
                changed = changed.changed() => changed.context("the daemon is stopping")?,
                ended = ended.changed() => ended.context("the daemon is stopping")?,
            }
        }
    }

    /// Add a reply to `finding` of a review of the workspace `selector`.
    pub async fn reply_to_finding(
        &self,
        selector: &str,
        finding: FindingId,
        body: String,
    ) -> Result<Review> {
        self.require_review_of(selector, finding.review).await?;
        crate::validate::agent_message(&body)?;
        let now = now();
        self.store
            .run(move |db| store::reply(db, finding, &body, now))
            .await?;
        self.review(finding.review).await
    }

    /// Resolve `finding` of a review of the workspace `selector` with
    /// `outcome`, or reopen it.
    pub async fn resolve_finding(
        &self,
        selector: &str,
        finding: FindingId,
        outcome: Option<Outcome>,
    ) -> Result<Review> {
        self.require_review_of(selector, finding.review).await?;
        self.store
            .run(move |db| store::resolve(db, finding, outcome.as_ref()))
            .await?;
        self.review(finding.review).await
    }

    /// The workspace `selector` names, which review `id` must be of.
    pub(crate) async fn require_review_of(&self, selector: &str, id: i64) -> Result<Workspace> {
        let workspace = self.workspace(selector).await?;
        let review = self.store.run(move |db| store::load(db, id)).await?;
        match review {
            Some(review) if review.workspace_id == workspace.id => Ok(workspace),
            Some(review) => bail!(
                "review {id} is of {}, not {}",
                review.workspace,
                workspace.name
            ),
            None => bail!("review {id} does not exist"),
        }
    }

    /// The workspace review `id` is of.
    pub(crate) async fn review_workspace(&self, id: i64) -> Result<String> {
        let review = self.store.run(move |db| store::load(db, id)).await?;
        Ok(review
            .with_context(|| format!("review {id} does not exist"))?
            .workspace_id)
    }

    /// Mark each review whose workspace's HEAD has moved past it as
    /// outdated; HEAD that cannot be read leaves it unknown.
    async fn annotate_reviews(&self, reviews: &mut [Review]) {
        let mut heads = HashMap::new();
        for review in reviews.iter_mut() {
            let Some(reviewed) = &review.head else {
                continue;
            };
            if !heads.contains_key(&review.workspace_id) {
                let head = match self.workspace(&review.workspace_id).await {
                    Ok(workspace) => crate::daemon::links::pr::current_head(&workspace)
                        .await
                        .ok(),
                    Err(_) => None,
                };
                heads.insert(review.workspace_id.clone(), head);
            }
            review.outdated = heads[&review.workspace_id]
                .as_ref()
                .map(|head| head != reviewed);
        }
    }

    /// Stop the workspace's running review, as its removal does; the removal
    /// then deletes its reviews.
    pub(crate) async fn stop_workspace_review(&self, workspace_id: &str) {
        let id = workspace_id.to_owned();
        let active = self.store.run(move |db| store::active(db, &id)).await;
        match active {
            Ok(Some((id, _))) => {
                if let Err(error) = self.cancel_review(None, Some(id), None).await {
                    log!("stopping review {id}: {error:#}");
                }
            }
            Ok(None) => {}
            Err(error) => log!("reviews of a removed workspace: {error:#}"),
        }
    }
}

/// The agent that reviews: the one asked for, else `[review] agent`, else
/// `agent.default`. It must have a review template.
fn reviewing_agent(settings: &Effective, agent: Option<Agent>) -> Result<String> {
    let agent = agent
        .or_else(|| settings.review.agent.clone())
        .or_else(|| settings.agent.default.clone())
        .context("no reviewer: pass --agent, or set review.agent or agent.default")?;
    let name = agent.name();
    let tool = settings
        .tools
        .get(name)
        .filter(|tool| tool.runs_as_agent)
        .with_context(|| format!("{name} is not an AI tool, so it cannot review"))?;
    ensure!(
        tool.launch.arguments.review.is_some(),
        "{name} has no review template; set agents.{name}.review to review with it"
    );
    Ok(name.to_owned())
}

fn now() -> i64 {
    i64::try_from(crate::time::unix_seconds()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests;
