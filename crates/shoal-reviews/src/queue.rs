//! Queueing and cancelling reviews, stopping those a removal ends, and the
//! refusals of a repository's move or removal while its reviews run.
use anyhow::{Context, Result, bail, ensure};
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{space_names::SpaceSelector, time::UnixTime};

use crate::{
    Reviews,
    agent::{Agent, ModelChoice},
    config::Effective,
    daemon::Daemon,
    log,
    reviews::{Review, ReviewId, ReviewTarget},
    store,
};

impl Reviews {
    /// Queue a review of the workspace by `agent`, else `[review] agent`, else
    /// `agent.default`, with `model` over the agent's configured model and
    /// effort, or return the one already queued. A running review refuses
    /// another.
    pub async fn queue(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        target: ReviewTarget,
        agent: Option<Agent>,
        model: ModelChoice,
    ) -> Result<Review> {
        let workspace = daemon.workspace(selector).await?;
        let settings = daemon.settings(&workspace).await?;
        let agent = reviewing_agent(&settings, agent)?;
        let (id, now) = (workspace.id.clone(), UnixTime::now());
        // A repository move, which refuses while a review is queued, holds
        // the registry gate, so no review is queued while one runs.
        let _registry = self.gates.lock_registry().await;
        let queued = self
            .store
            .run(move |db| store::queue(db, &id, &target, &agent, &model, now))
            .await?;
        self.queued.notify_one();
        self.review(daemon, queued.id()).await
    }

    /// Cancel review `id`, else the workspace's queued or running one, for a
    /// process of workspace `by` when one asked.
    pub async fn cancel(
        &self,
        daemon: &dyn Daemon,
        selector: Option<&SpaceSelector>,
        id: Option<ReviewId>,
        by: Option<SpaceId>,
    ) -> Result<Review> {
        let id = match (id, selector) {
            (Some(id), _) => id,
            (None, Some(selector)) => self.active(daemon, selector).await?,
            (None, None) => bail!("name a review or a workspace"),
        };
        let now = UnixTime::now();
        let cancelled = self
            .store
            .run(move |db| store::cancel(db, id, by.as_ref(), now))
            .await?;
        ensure!(cancelled, "review {id} has already ended");
        if let Some(cancel) = self.cancels.lock().unwrap().get(&id) {
            cancel.send_replace(true);
        }
        self.deliver(daemon, id).await;
        self.review(daemon, id).await
    }

    /// The ID of the workspace's queued or running review.
    async fn active(&self, daemon: &dyn Daemon, selector: &SpaceSelector) -> Result<ReviewId> {
        let workspace = daemon.workspace(selector).await?;
        let id = workspace.id.clone();
        let active = self.store.run(move |db| store::active(db, &id)).await?;
        let (id, _) = active
            .with_context(|| format!("{} has no queued or running review", workspace.name))?;
        Ok(id)
    }

    /// Refuse while a review of the repository is queued or running, or
    /// [`Self::ensure_no_runs_left`] refuses.
    pub async fn ensure_none_in(
        &self,
        daemon: &dyn Daemon,
        repository_id: &RepositoryId,
    ) -> Result<()> {
        let (id, records) = (repository_id.to_owned(), self.records.clone());
        let active = (self.store)
            .run(move |db| store::active_in(db, &*records, &id))
            .await?;
        ensure!(
            active.is_empty() && !self.holds_checkout_of(repository_id, false),
            "a review of the repository is queued or running; wait for it or cancel it"
        );
        self.ensure_no_runs_left(daemon, repository_id).await
    }

    /// Refuse while a review that ended, even with its workspace, still has
    /// a checkout, a worktree of the repository, or left its run in the
    /// repository's directory, as one whose reviewer's processes may still
    /// run, which only a restarted daemon removes.
    pub async fn ensure_no_runs_left(
        &self,
        daemon: &dyn Daemon,
        repository_id: &RepositoryId,
    ) -> Result<()> {
        let left = self.runs_left(daemon, repository_id).await?;
        ensure!(
            !self.holds_checkout_of(repository_id, true) && !left,
            "an ended review left a checkout of the repository that could not be removed; \
             restart the daemon with `shoal daemon restart` to remove it"
        );
        Ok(())
    }

    /// Cancel the reviews of the repository's workspaces and wait until their
    /// runs have ended and removed their checkouts, which are worktrees of
    /// the repository its removal would otherwise refuse.
    pub async fn stop_in_repository(&self, daemon: &dyn Daemon, repository_id: &RepositoryId) {
        let (id, records) = (repository_id.to_owned(), self.records.clone());
        let active = (self.store).run(move |db| store::active_in(db, &*records, &id));
        let active = match active.await {
            Ok(active) => active,
            Err(error) => {
                log!("reviews of a removed repository: {error:#}");
                return;
            }
        };
        let mut ended = self.ended.subscribe();
        for id in &active {
            if let Err(error) = self.cancel(daemon, None, Some(*id), None).await {
                log!("stopping review {id}: {error:#}");
            }
        }
        let running = || {
            let cancels = self.cancels.lock().unwrap();
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
    pub async fn stop_in_workspace(&self, daemon: &dyn Daemon, workspace_id: &SpaceId) {
        let id = workspace_id.to_owned();
        let active = self.store.run(move |db| store::active(db, &id)).await;
        match active {
            Ok(Some((id, _))) => {
                if let Err(error) = self.cancel(daemon, None, Some(id), None).await {
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
fn reviewing_agent(settings: &Effective, agent: Option<Agent>) -> Result<Agent> {
    let agent = agent
        .or_else(|| settings.review.agent.clone())
        .or_else(|| settings.agent.default.clone())
        .context("no reviewer: pass --agent, or set review.agent or agent.default")?;
    settings.check_reviewer(&agent)?;
    Ok(agent)
}
