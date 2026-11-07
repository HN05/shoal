//! Local reviews of workspaces and PRs: queueing them, running each once in
//! a detached checkout, and recording what the reviewer found and how the
//! workspace's agent answered it.
use std::collections::HashMap;

use anyhow::{Context, Result, bail, ensure};
use tokio::sync::watch;

use super::{log, workspace::Manager};
use crate::client::space_names::SpaceSelector;
use crate::{
    agent::Agent,
    config::Effective,
    reviews::{FindingId, Outcome, Review, ReviewTarget},
};

mod delivery;
mod false_positives;
pub(crate) mod gate;
mod history;
mod post;
mod reviewer;
mod run;
pub(crate) mod store;

pub(crate) use run::run;

/// The daemon's running reviews.
pub(crate) struct Reviews {
    /// Wakes the scheduler when a review is queued.
    queued: tokio::sync::Notify,
    /// Stops each review that has started to run.
    cancels: std::sync::Mutex<HashMap<i64, std::sync::Arc<watch::Sender<bool>>>>,
    /// The repository of each review whose checkout, a worktree of it, may
    /// exist, which outlives the review's records when its workspace goes.
    checkouts: std::sync::Mutex<HashMap<i64, HeldCheckout>>,
    /// Counts the runs that ended, for waits on one.
    ended: watch::Sender<u64>,
}

impl Default for Reviews {
    fn default() -> Self {
        Self {
            queued: Default::default(),
            cancels: Default::default(),
            checkouts: Default::default(),
            ended: watch::channel(0).0,
        }
    }
}

impl Reviews {
    /// Record that review `id` may have a checkout of `repository` until the
    /// returned value is dropped, once it is removed.
    fn hold_checkout(&self, id: i64, repository: &str) -> CheckoutHold<'_> {
        self.checkouts.lock().unwrap().insert(
            id,
            HeldCheckout {
                repository: repository.to_owned(),
                kept: false,
            },
        );
        CheckoutHold { reviews: self, id }
    }

    /// Whether a review's checkout of `repository` may exist that its run
    /// still removes, or, when `kept`, that only a restarted daemon removes.
    fn holds_checkout_of(&self, repository: &str, kept: bool) -> bool {
        self.checkouts
            .lock()
            .unwrap()
            .values()
            .any(|held| held.repository == repository && held.kept == kept)
    }
}

/// A review's checkout that may exist.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HeldCheckout {
    repository: String,
    /// Whether its run ended without removing it.
    kept: bool,
}

/// A review's possible checkout, until it is removed.
struct CheckoutHold<'a> {
    reviews: &'a Reviews,
    id: i64,
}

impl CheckoutHold<'_> {
    /// Keep holding a checkout that could not be removed, until the daemon
    /// restarts and removes what its runs left.
    fn keep(self) {
        if let Some(held) = self.reviews.checkouts.lock().unwrap().get_mut(&self.id) {
            held.kept = true;
        }
        std::mem::forget(self);
    }
}

impl Drop for CheckoutHold<'_> {
    fn drop(&mut self) {
        self.reviews.checkouts.lock().unwrap().remove(&self.id);
    }
}

impl Manager {
    /// Queue a review of the workspace by `agent`, else `[review] agent`, else
    /// `agent.default`, with `model` over the agent's configured model and
    /// effort, or return the one already queued. A running review refuses
    /// another.
    pub async fn queue_review(
        &self,
        selector: &SpaceSelector,
        target: ReviewTarget,
        agent: Option<Agent>,
        model: crate::agent::ModelChoice,
    ) -> Result<Review> {
        let workspace = self.workspace(selector).await?;
        let settings = self.workspace_settings(&workspace).await?;
        let agent = reviewing_agent(&settings, agent)?;
        let (id, now) = (workspace.id.clone(), now());
        // A repository move, which refuses while a review is queued, holds
        // the registry gate, so no review is queued while one runs.
        let _registry = self.gates.lock_registry().await;
        let queued = self
            .store
            .run(move |db| store::queue(db, &id, &target, &agent, &model, now))
            .await?;
        self.reviews.queued.notify_one();
        self.review(queued.id()).await
    }

    /// Cancel review `id`, else the workspace's queued or running one, for a
    /// process of workspace `by` when one asked.
    pub async fn cancel_review(
        &self,
        selector: Option<&SpaceSelector>,
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
        self.deliver_review(id).await;
        self.review(id).await
    }

    /// The ID of the workspace's queued or running review.
    async fn active_review(&self, selector: &SpaceSelector) -> Result<i64> {
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
    pub async fn latest_review(&self, selector: &SpaceSelector) -> Result<Review> {
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
        workspace: Option<&SpaceSelector>,
        repository: Option<crate::repository::Selector>,
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

    /// Add a reply to `finding`.
    pub async fn reply_to_finding(&self, finding: FindingId, body: String) -> Result<Review> {
        crate::validate::agent_message(&body)?;
        let now = now();
        self.store
            .run(move |db| store::reply(db, finding, &body, now))
            .await?;
        self.review(finding.review).await
    }

    /// Resolve `finding` with `outcome`, or reopen it, and keep it among its
    /// repository's false positives only while it is resolved as one. The
    /// file changes first and goes back when the outcome cannot be stored,
    /// so an interrupted resolve leaves the file with the newer intent.
    pub async fn resolve_finding(
        &self,
        finding: FindingId,
        outcome: Option<Outcome>,
    ) -> Result<Review> {
        // Moves and removal take this gate too. Holding it over both steps
        // keeps concurrent resolves in one order in the outcome and the file.
        let registry = self.gates.lock_registry().await;
        let resolved = self.stored_review(finding.review).await?;
        let previous = self
            .keep_false_positive(&resolved, finding, outcome.as_ref())
            .await?;
        if let Err(error) = self.record_outcome(finding, outcome).await {
            if let Some(previous) = previous {
                previous.restore().await?;
            }
            return Err(error);
        }
        drop(registry);
        self.review(finding.review).await
    }

    /// Store `outcome` as `finding`'s, or reopen it without one.
    async fn record_outcome(&self, finding: FindingId, outcome: Option<Outcome>) -> Result<()> {
        self.store
            .run(move |db| store::resolve(db, finding, outcome.as_ref()))
            .await
    }

    /// Review `id` as stored, without what its workspace says of it.
    async fn stored_review(&self, id: i64) -> Result<Review> {
        let review = self.store.run(move |db| store::load(db, id)).await?;
        review.with_context(|| format!("review {id} does not exist"))
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
                let head = match self.workspace_by_id(&review.workspace_id).await {
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

    /// Refuse while a review of the repository is queued or running, or a
    /// review that ended, even with its workspace, still has a checkout, a
    /// worktree of the repository.
    pub(crate) async fn ensure_no_reviews_in(&self, repository_id: &str) -> Result<()> {
        let id = repository_id.to_owned();
        let active = self.store.run(move |db| store::active_in(db, &id)).await?;
        ensure!(
            active.is_empty() && !self.reviews.holds_checkout_of(repository_id, false),
            "a review of the repository is queued or running; wait for it or cancel it"
        );
        ensure!(
            !self.reviews.holds_checkout_of(repository_id, true),
            "an ended review left a checkout of the repository that could not be removed; \
             restart the daemon with `shoal daemon restart` to remove it"
        );
        Ok(())
    }

    /// Cancel the reviews of the repository's workspaces and wait until their
    /// runs have ended and removed their checkouts, which are worktrees of
    /// the repository its removal would otherwise refuse.
    pub(crate) async fn stop_repository_reviews(&self, repository_id: &str) {
        let id = repository_id.to_owned();
        let active = match self.store.run(move |db| store::active_in(db, &id)).await {
            Ok(active) => active,
            Err(error) => {
                log!("reviews of a removed repository: {error:#}");
                return;
            }
        };
        let mut ended = self.reviews.ended.subscribe();
        for id in &active {
            if let Err(error) = self.cancel_review(None, Some(*id), None).await {
                log!("stopping review {id}: {error:#}");
            }
        }
        let running = || {
            let cancels = self.reviews.cancels.lock().unwrap();
            active.iter().any(|id| cancels.contains_key(id))
        };
        while running() {
            if ended.changed().await.is_err() {
                return;
            }
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
    settings.check_reviewer(agent.name())?;
    Ok(agent.name().to_owned())
}

fn now() -> i64 {
    i64::try_from(crate::time::unix_seconds()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests;
