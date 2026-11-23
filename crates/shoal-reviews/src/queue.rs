//! Queueing and cancelling reviews, stopping those a removal ends and
//! those the machine's sleep would cut off, and the refusals of a
//! repository's move or removal while its reviews run.
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
    written::Writer,
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

    /// Cancel review `id`, else the workspace's pending review that the
    /// space `by`, or the user without one, writes, else its queued or
    /// running one, for a process of workspace `by` when one asked. A
    /// pending review goes with its threads.
    pub async fn cancel(
        &self,
        daemon: &dyn Daemon,
        selector: Option<&SpaceSelector>,
        id: Option<ReviewId>,
        by: Option<SpaceId>,
    ) -> Result<Review> {
        let writer = Writer::of(by.as_ref());
        let id = match (id, selector) {
            (Some(id), _) => id,
            (None, Some(selector)) => self.cancelled_of(daemon, selector, &writer).await?,
            (None, None) => bail!("name a review or a workspace"),
        };
        match self.writer_of(id).await? {
            Some(_) => self.cancel_written(daemon, id, &writer).await,
            None => self.cancel_run(daemon, id, by).await,
        }
    }

    /// The review a cancel without an ID ends, of the workspace `selector`
    /// names: `writer`'s pending one, else its queued or running one.
    async fn cancelled_of(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        writer: &Writer,
    ) -> Result<ReviewId> {
        let found = self.to_cancel(daemon, selector, writer).await?;
        found.with_context(|| {
            format!("{selector} has no queued or running review, nor one of yours pending")
        })
    }

    /// `writer`'s pending review of the workspace `selector` names, else
    /// its queued or running one, which a cancel without an ID ends.
    pub async fn to_cancel(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        writer: &Writer,
    ) -> Result<Option<ReviewId>> {
        if let Some(id) = self.pending_of(daemon, selector, writer).await? {
            return Ok(Some(id));
        }
        let workspace = daemon.workspace(selector).await?;
        self.active(&workspace.id).await
    }

    /// Cancel the run of Shoal's review `id`, queued or running, for a
    /// process of workspace `by` when one asked.
    async fn cancel_run(
        &self,
        daemon: &dyn Daemon,
        id: ReviewId,
        by: Option<SpaceId>,
    ) -> Result<Review> {
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

    /// The ID of the workspace `id`'s queued or running review.
    async fn active(&self, id: &SpaceId) -> Result<Option<ReviewId>> {
        let id = id.clone();
        let active = self.store.run(move |db| store::active(db, &id)).await?;
        Ok(active.map(|(id, _)| id))
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

    /// Cancel the reviews of the repository's workspaces and the answers
    /// made for them, and wait until their runs have ended and removed their
    /// checkouts, which are worktrees of the repository its removal would
    /// otherwise refuse.
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
        let answers = self.stop_answers(|run| run.repository == *repository_id);
        let running = || {
            let cancels = self.cancels.lock().unwrap();
            active.iter().any(|id| cancels.contains_key(id)) || self.answering_any(&answers)
        };
        while running() {
            if ended.changed().await.is_err() {
                return;
            }
        }
    }

    /// Stop the running reviews, which the machine's sleep would cut off,
    /// queueing each again to run once the agent queue admits it after the
    /// machine wakes.
    pub async fn stop_for_sleep(&self) {
        let running = match self.store.read(|db| store::running(db)).await {
            Ok(running) => running,
            Err(error) => return log!("reviews to stop for sleep: {error:#}"),
        };
        let cancels = self.cancels.lock().unwrap();
        for id in running {
            if let Some(cancel) = cancels.get(&id) {
                self.slept.lock().unwrap().insert(id);
                cancel.send_replace(true);
            }
        }
    }

    /// Stop the workspace's running review and the answers made for it, as
    /// its removal does; the removal then deletes its reviews.
    pub async fn stop_in_workspace(&self, daemon: &dyn Daemon, workspace_id: &SpaceId) {
        self.stop_answers(|run| run.workspace == *workspace_id);
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
