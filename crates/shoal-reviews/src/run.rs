//! Running queued reviews: each starts at once, takes the workspace's HEAD,
//! runs its reviewer in a detached checkout at the place of the workspace's
//! or PR's reviews, under a scope that writes only that review, falls back
//! to a second reviewer once, dropping what a failed run wrote, and fails
//! the review when no run submitted it.
use std::{collections::HashSet, path::Path, sync::Arc};

use anyhow::{Context, Result, anyhow};
use shoal_forge::activity::view::ViewRequests;
use shoal_ids::agent_session_ids::AgentSessionId;
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::time::UnixTime;
use shoal_refs::{BranchName, CommitId};
use tokio::{sync::watch, task::JoinSet};

use crate::{
    Reviews,
    agent::{Agent, ModelChoice},
    checkouts::CheckoutHold,
    config::{Effective, templates},
    daemon::{Admission, AwaitedPost, Daemon},
    false_positives, findings, git, history, log,
    model::{Repository, Workspace},
    oneshot,
    reviewer::{self, Ended, Exited, Files, Reviewer},
    reviews::{FailureReason, Review, ReviewId, ReviewState, ReviewTarget},
    scope::RunScope,
    shoal_dirs::{Contents, ShoalDir},
    store,
};

/// Start every queued review and every answer a finding's thread waits
/// for, now and whenever one is queued or posted; each then waits in the
/// agent queue for admission.
pub async fn run(reviews: Arc<Reviews>, daemon: Arc<dyn Daemon>) {
    let mut running = JoinSet::new();
    let mut started = HashSet::new();
    loop {
        let queued = reviews.queued.notified();
        tokio::pin!(queued);
        queued.as_mut().enable();
        match waiting(&reviews).await {
            Ok(jobs) => {
                for job in jobs.into_iter().filter(|job| started.insert(*job)) {
                    let (reviews, daemon) = (reviews.clone(), daemon.clone());
                    running.spawn(async move {
                        match job {
                            Job::Review(id) => reviews.run_review(&*daemon, id).await,
                            Job::Answer(post) => reviews.answer(&*daemon, post).await,
                        }
                        job
                    });
                }
            }
            Err(error) => log!("queued reviews: {error:#}"),
        }
        tokio::select! {
            () = &mut queued => {}
            Some(ended) = running.join_next(), if !running.is_empty() => {
                if let Ok(job) = ended {
                    started.remove(&job);
                }
            }
        }
    }
}

/// What the scheduler runs: a queued review, or the reviewer's answer to
/// the latest post in a finding's thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Job {
    Review(ReviewId),
    Answer(AwaitedPost),
}

/// The queued reviews, then the posts in findings' threads that wait for
/// an answer the reviewer has not yet run for.
async fn waiting(reviews: &Reviews) -> Result<Vec<Job>> {
    let records = reviews.records.clone();
    let (queued, awaited) = (reviews.store)
        .run(move |db| Ok((store::queued(db)?, records.awaiting_answers(db)?)))
        .await?;
    let unanswered = reviews.unanswered(&awaited);
    let queued = queued.into_iter().map(Job::Review);
    Ok(queued
        .chain(unanswered.into_iter().map(Job::Answer))
        .collect())
}

/// How one pass of a review ended.
enum Pass {
    Ended,
    /// Protection stopped its run and queued the review again.
    Stopped,
    /// The machine's sleep stopped its run and queued the review again.
    Slept,
}

/// A run in the place of a review's subject: the review, its workspace and
/// settings, the commits it reads, and the run it holds there.
pub(crate) struct InPlace<'a> {
    pub(crate) review: &'a Review,
    pub(crate) workspace: &'a Workspace,
    pub(crate) settings: &'a Effective,
    pub(crate) commits: &'a Commits,
    pub(crate) held: &'a HeldRun<'a>,
}

/// One agent's attempt at a run, for a review.
struct Attempt<'a> {
    run: &'a Run<'a>,
    review: &'a Review,
    agent: &'a Agent,
}

/// What every reviewer of a run gets.
struct Run<'a> {
    daemon: &'a dyn Daemon,
    runs: &'a oneshot::Runs,
    /// The reviewed workspace, whose scope the reviewer holds.
    workspace: &'a Workspace,
    settings: &'a Effective,
    /// The workspace's configured variables.
    configured: std::collections::BTreeMap<String, std::ffi::OsString>,
    instructions: String,
    files: &'a Files,
}

/// The commits a run reviews.
pub(crate) struct Commits {
    head: CommitId,
    base: CommitId,
}

impl Reviews {
    /// Run queued review `id` to its end. A failure to prepare it fails it,
    /// as the reviewer could not run.
    async fn run_review(&self, daemon: &dyn Daemon, id: ReviewId) {
        loop {
            let (cancel, mut cancelled) = watch::channel(false);
            let cancel = Arc::new(cancel);
            self.cancels.lock().unwrap().insert(id, cancel.clone());
            match self.review_once(daemon, id, &cancel, &mut cancelled).await {
                Ok(Pass::Stopped) => {
                    log!("review {id}: protection stopped it; it runs again once load recovers");
                }
                Ok(Pass::Slept) => {
                    log!(
                        "review {id}: the machine's sleep stopped it; it runs again once it wakes"
                    );
                }
                Ok(Pass::Ended) => break,
                Err(error) => {
                    self.fail_unprepared(daemon, id, error).await;
                    break;
                }
            }
        }
        self.cancels.lock().unwrap().remove(&id);
        self.slept.lock().unwrap().remove(&id);
        self.ended.send_modify(|ended| *ended += 1);
    }

    /// Fail review `id`, which could not be prepared: for its repository's
    /// unreadable false positives, else as its reviewer could not run.
    async fn fail_unprepared(&self, daemon: &dyn Daemon, id: ReviewId, error: anyhow::Error) {
        log!("review {id}: {error:#}");
        let reason = match error.downcast_ref::<false_positives::Unreadable>() {
            Some(_) => FailureReason::InvalidFalsePositives,
            None => FailureReason::ExecutionFailed,
        };
        let _operation = self.gates.operation().await;
        let now = UnixTime::now();
        let failed = self
            .store
            .run(move |db| store::fail(db, id, None, reason, now))
            .await;
        match failed {
            Ok(true) => self.deliver(daemon, id).await,
            Ok(false) => {}
            Err(error) => log!("review {id}: {error:#}"),
        }
    }

    /// Run queued review `id` once the agent queue admits it, which protection
    /// stops by sending `cancel`; a review whose run it stopped asks for one
    /// that waits for healthy readings.
    async fn review_once(
        &self,
        daemon: &dyn Daemon,
        id: ReviewId,
        cancel: &Arc<watch::Sender<bool>>,
        cancelled: &mut watch::Receiver<bool>,
    ) -> Result<Pass> {
        let Some(review) = self.queued_review(id).await? else {
            return Ok(Pass::Ended);
        };
        let workspace = daemon.workspace_by_id(&review.workspace_id).await?;
        let Some(admission) =
            (self.await_admission(daemon, id, &workspace, cancel, cancelled)).await?
        else {
            return Ok(Pass::Ended);
        };
        // The workspace may have moved while the review waited, as a fork
        // PR's moves to the PR's new head.
        let workspace = daemon.workspace_by_id(&review.workspace_id).await?;
        // Its configuration and commits are read only from the worktree
        // that is this workspace's.
        daemon.verify_worktree(&workspace).await?;
        let settings = daemon.settings_with_templates(&workspace).await?;
        let commits = review_commits(daemon, &workspace, &review.target).await?;
        let repository = daemon.repository(workspace.repository()).await?;
        let place = run_files(&repository, &review)?;
        let held = self.hold_run(id, &repository.id, place).await?;
        if !self.start_review(id, &commits).await? {
            return Ok(Pass::Ended);
        }
        let in_place = InPlace {
            review: &review,
            workspace: &workspace,
            settings: &settings,
            commits: &commits,
            held: &held,
        };
        let ended = self.run_in_checkout(daemon, &in_place, cancelled).await;
        let left_running = matches!(ended, Ok((_, Ended::LeftRunning)));
        release_run(id, &held.files, held.checkout, admission, left_running).await;
        let (reviewer, ended) = ended?;
        self.record_end(daemon, id, &reviewer, ended).await
    }

    /// Review `id` while it is queued.
    async fn queued_review(&self, id: ReviewId) -> Result<Option<Review>> {
        let review = self.load(id).await?;
        Ok(review.filter(|review| review.state == ReviewState::Queued))
    }

    /// Wait for review `id`'s admission in its workspace's repository, which
    /// protection takes back by sending `cancel`; none once `cancelled`, as a
    /// cancel or the workspace's removal recorded the review's end.
    pub(crate) async fn await_admission<'a>(
        &self,
        daemon: &'a dyn Daemon,
        id: ReviewId,
        workspace: &'a Workspace,
        cancel: &Arc<watch::Sender<bool>>,
        cancelled: &mut watch::Receiver<bool>,
    ) -> Result<Option<Box<dyn Admission + 'a>>> {
        let after_stop = (self.store)
            .run(move |db| store::waits_for_recovery(db, id))
            .await?;
        let stop = cancel.clone();
        let stop = Box::new(move || {
            stop.send_replace(true);
        });
        let repository = workspace.repository();
        let mut admission = (daemon.admit(repository, &workspace.id, after_stop, stop)).await?;
        tokio::select! {
            () = admission.admitted() => Ok(Some(admission)),
            _ = cancelled.wait_for(|cancelled| *cancelled) => Ok(None),
        }
    }

    /// Hold the run of review `id` of `repository` at `place`, its
    /// subject's, once no earlier run there holds it, as a cancelled
    /// review's does while it ends; while a run that could not be removed
    /// keeps the place, until a restarted daemon removes it, the review runs
    /// beside it instead.
    pub(crate) async fn hold_run(
        &self,
        id: ReviewId,
        repository: &RepositoryId,
        place: Files,
    ) -> Result<HeldRun<'_>> {
        let mut ended = self.ended.subscribe();
        loop {
            match self.try_hold_run(id, repository, &place.directory) {
                Ok(checkout) if !place.directory.exists() => {
                    return Ok(HeldRun {
                        files: place,
                        checkout,
                        at_place: true,
                    });
                }
                Ok(checkout) => {
                    drop(checkout);
                    break;
                }
                Err(true) => break,
                Err(false) => ended.changed().await.context("the daemon is stopping")?,
            }
        }
        log!(
            "review {id}: an earlier review left its run at {} that could not be removed; this \
             review runs beside it in a new session until `shoal daemon restart` removes it",
            place.directory.display()
        );
        let files = run_beside(&place, id);
        let checkout = (self.try_hold_run(id, repository, &files.directory))
            .map_err(|_| anyhow!("{} is in use", files.directory.display()))?;
        Ok(HeldRun {
            files,
            checkout,
            at_place: false,
        })
    }

    /// Record that review `id` started on `commits`; false when it is no
    /// longer queued.
    async fn start_review(&self, id: ReviewId, commits: &Commits) -> Result<bool> {
        let (head, base, now) = (commits.head.clone(), commits.base.clone(), UnixTime::now());
        self.store
            .run(move |db| store::start(db, id, &head, &base, now))
            .await
    }

    /// Check the reviewed commit out in the run `place` holds and run the
    /// reviewers there, continuing the subject's session; returns the agent
    /// that produced the review and how it ended.
    async fn run_in_checkout(
        &self,
        daemon: &dyn Daemon,
        place: &InPlace<'_>,
        cancelled: &mut watch::Receiver<bool>,
    ) -> Result<(Agent, Ended<Exited>)> {
        let instructions = self.review_instructions(daemon, place).await?;
        let session = self.review_session(place.review, place.settings).await;
        let agent = (place.review.agent()).context("only Shoal's reviews run a reviewer")?;
        (self.run_reviewer(daemon, place, agent, instructions, session, cancelled)).await
    }

    /// Check the commit `place` reads out in the run it holds and run
    /// `agent` there with `instructions`, then the fallback agent once,
    /// resuming `session` when the run is at the subject's place, where its
    /// session is found; returns the agent that produced the result and how
    /// it ended.
    pub(crate) async fn run_reviewer<O: reviewer::Output>(
        &self,
        daemon: &dyn Daemon,
        place: &InPlace<'_>,
        agent: &Agent,
        instructions: String,
        session: Option<AgentSessionId>,
        cancelled: &mut watch::Receiver<bool>,
    ) -> Result<(Agent, Ended<O>)> {
        let InPlace {
            review,
            workspace,
            settings,
            commits,
            held,
        } = place;
        let files = &held.files;
        prepare_checkout(
            daemon,
            review.id,
            workspace,
            &commits.head,
            files,
            O::SCHEMA,
        )
        .await?;
        let run = Run {
            runs: &self.runs,
            daemon,
            workspace,
            settings,
            configured: daemon.environment(workspace, settings).await?,
            instructions,
            files,
        };
        let attempt = Attempt {
            run: &run,
            review,
            agent,
        };
        // A run elsewhere finds no session there, and no later review its own.
        if !held.at_place {
            let (agent, ended) = self.review_with_fallback(&attempt, None, cancelled).await;
            return Ok((agent, ended.without_session()));
        }
        Ok(self
            .review_with_fallback(&attempt, session, cancelled)
            .await)
    }

    /// `attempt`'s own agent at its run, resuming `session` when given, then
    /// once the fallback agent's after any failure but a timeout, each
    /// after what the failed one wrote is dropped; returns the agent that
    /// produced the result and how it ended.
    async fn review_with_fallback<O: reviewer::Output>(
        &self,
        attempt: &Attempt<'_>,
        session: Option<AgentSessionId>,
        cancelled: &mut watch::Receiver<bool>,
    ) -> (Agent, Ended<O>) {
        let Attempt { run, review, agent } = attempt;
        let settings = run.settings;
        let model = settings.model(agent, review.model().cloned().unwrap_or_default());
        let ended = self
            .review_by_agent(run, review.id, agent, &model, session, cancelled)
            .await;
        let fallback = settings
            .review
            .fallback_agent
            .clone()
            .filter(|fallback| fallback != *agent && !settings.turned_off(fallback));
        match (ended, fallback) {
            (Ended::Failed(reason), Some(fallback)) if reason != FailureReason::TimedOut => {
                log!(
                    "review {}: {agent} failed ({reason}); {fallback} reviews instead",
                    review.id
                );
                let model = settings.model(&fallback, Default::default());
                let ended =
                    (self.attempt(run, review.id, &fallback, &model, None, cancelled)).await;
                (fallback, ended)
            }
            (ended, _) => ((*agent).clone(), ended),
        }
    }

    /// The session `review`'s agent resumes: the one the latest earlier
    /// review of its workspace or PR recorded, when that agent reported it
    /// and can resume one. A change of agent, or a fallback that reviewed
    /// last, starts a new session.
    async fn review_session(
        &self,
        review: &Review,
        settings: &Effective,
    ) -> Option<AgentSessionId> {
        let agent = review.agent()?;
        if !resumes(settings, agent) {
            return None;
        }
        let id = review.id;
        match self.store.run(move |db| store::session(db, id)).await {
            Ok(saved) => saved.and_then(|(saved, session)| (saved == *agent).then_some(session)),
            Err(error) => {
                log!("review {id}: its reviewer's session: {error:#}");
                None
            }
        }
    }

    /// The attempt of review `id`'s own `agent` at `run`, resuming `session`
    /// when there is one, and in a new session when there is none or
    /// resuming it failed for a reason other than running too long.
    async fn review_by_agent<O: reviewer::Output>(
        &self,
        run: &Run<'_>,
        id: ReviewId,
        agent: &Agent,
        model: &ModelChoice,
        session: Option<AgentSessionId>,
        cancelled: &mut watch::Receiver<bool>,
    ) -> Ended<O> {
        if let Some(session) = session {
            match self
                .attempt(run, id, agent, model, Some(&session), cancelled)
                .await
            {
                Ended::Returned {
                    output,
                    session: reported,
                } => {
                    let session = reported.or(Some(session));
                    return Ended::Returned { output, session };
                }
                Ended::Failed(reason) if reason != FailureReason::TimedOut => log!(
                    "review {id}: resuming {agent}'s session failed ({reason}); it starts a new one"
                ),
                ended => return ended,
            }
        }
        self.attempt(run, id, agent, model, None, cancelled).await
    }

    /// One reviewer's attempt at `run` for review `id`: agent `agent` with
    /// `model`, resuming `session` when given, under a scope of its own
    /// that ends with it. What a review's run that did not produce the
    /// review wrote is dropped.
    async fn attempt<O: reviewer::Output>(
        &self,
        run: &Run<'_>,
        id: ReviewId,
        agent: &Agent,
        model: &ModelChoice,
        session: Option<&AgentSessionId>,
        cancelled: &mut watch::Receiver<bool>,
    ) -> Ended<O> {
        self.record_attempt(id, agent).await;
        let scope = RunScope::of(&self.operations, &run.workspace.id, O::access(id));
        let reviewer = reviewer_for::<O>(run, agent, model, session, &self.paths, &scope);
        let ended = match reviewer {
            Ok(reviewer) => reviewer::run(&reviewer, cancelled).await,
            Err(error) => {
                log!("review by {agent}: {error:#}");
                Ended::Failed(FailureReason::ExecutionFailed)
            }
        };
        drop(scope);
        let ended = O::settled(ended, self.finished(id).await);
        if !matches!(ended, Ended::Returned { .. }) {
            self.drop_run_writes(run, id).await;
        }
        ended
    }

    /// Record that `agent` reviews running review `id` now, as the run that
    /// submits it does.
    async fn record_attempt(&self, id: ReviewId, agent: &Agent) {
        let agent = agent.clone();
        let recorded = (self.store).run(move |db| store::attempt(db, id, &agent));
        if let Err(error) = recorded.await {
            log!("review {id}: {error:#}");
        }
    }

    /// Whether review `id` finished, as a run that submitted it leaves it.
    async fn finished(&self, id: ReviewId) -> bool {
        let state = self.store.run(move |db| store::state(db, id)).await;
        matches!(state, Ok(Some(ReviewState::Finished)))
    }

    /// Drop what review `id`'s `run` wrote while the review has not
    /// finished, waking what waits for its workspace's threads.
    async fn drop_run_writes(&self, run: &Run<'_>, id: ReviewId) {
        let records = self.records.clone();
        let dropped = (self.store).write(move |tx| store::drop_run_writes(tx, &*records, id));
        match dropped.await {
            Ok(true) => run.daemon.review_threads_changed(run.workspace).await,
            Ok(false) => {}
            Err(error) => log!("review {id}: {error:#}"),
        }
    }

    /// Record how the run ended under an operation guard: the session of a
    /// run that submitted the review, which its submit posted and
    /// delivered, or the review's failure, which it delivers. A cancel
    /// already recorded and delivered it; a run that ended cancelled while
    /// still running was stopped by protection or the machine's sleep, and
    /// its review is queued again.
    async fn record_end(
        &self,
        daemon: &dyn Daemon,
        id: ReviewId,
        reviewer: &Agent,
        ended: Ended<Exited>,
    ) -> Result<Pass> {
        // A managed update waits for the review's delivery, which a restart
        // would not redo once the failure is recorded.
        let _operation = self.gates.operation().await;
        let reason = match ended {
            Ended::Cancelled => return self.requeue_stopped(id).await,
            Ended::Returned { session, .. } => {
                if let Some(session) = session {
                    (self.store)
                        .run(move |db| store::continue_session(db, id, &session))
                        .await?;
                }
                return Ok(Pass::Ended);
            }
            Ended::Failed(reason) => reason,
            Ended::LeftRunning => FailureReason::ExecutionFailed,
        };
        let (reviewer, now) = (reviewer.to_owned(), UnixTime::now());
        let failed = self
            .store
            .run(move |db| store::fail(db, id, Some(&reviewer), reason, now))
            .await?;
        if failed {
            self.deliver(daemon, id).await;
        }
        Ok(Pass::Ended)
    }

    /// Queue review `id`, whose run was stopped while it ran, again: to wait
    /// for healthy readings unless the machine's sleep or pause stopped it.
    async fn requeue_stopped(&self, id: ReviewId) -> Result<Pass> {
        let slept = self.slept.lock().unwrap().remove(&id);
        let requeued = (self.store)
            .run(move |db| store::requeue(db, id, !slept))
            .await?;
        Ok(match (requeued, slept) {
            (false, _) => Pass::Ended,
            (true, false) => Pass::Stopped,
            (true, true) => Pass::Slept,
        })
    }

    /// The review's instructions for the checkout `place` holds, then its
    /// history.
    async fn review_instructions(
        &self,
        daemon: &dyn Daemon,
        place: &InPlace<'_>,
    ) -> Result<String> {
        let InPlace {
            review,
            workspace,
            settings,
            commits,
            held,
        } = place;
        let checkout = held.files.checkout();
        let instructions = templates::review_instructions(
            (settings.rules.get(crate::config::modes::REVIEW_RULE)).map(|rule| rule.text.as_str()),
            &templates::ReviewFields {
                workspace: workspace.directory_name(),
                branch: workspace
                    .branch()
                    .map(BranchName::as_str)
                    .unwrap_or_default(),
                path: &checkout.to_string_lossy(),
                base: commits.base.as_str(),
            },
        );
        let (id, workspace_id) = (review.id, workspace.id.clone());
        let false_positives = findings::listed(daemon, workspace.repository()).await?;
        let records = self.records.clone();
        let earlier = self
            .store
            .run(move |db| store::earlier(db, &*records, &workspace_id, id))
            .await?;
        let pull = match &review.target {
            ReviewTarget::Pr { url, .. } => pull_discussion(daemon, workspace, url).await,
            ReviewTarget::Workspace => None,
        };
        // The history fills what the instructions leave of the prompt.
        let room = history::PROMPT_LIMIT.saturating_sub(instructions.len() + 2);
        let history = history::history(&false_positives, &earlier, pull.as_ref(), room);
        Ok(match history.is_empty() {
            true => instructions,
            false => format!("{instructions}\n\n{history}"),
        })
    }

    /// Remove the checkouts and output that reviews a daemon stop interrupted
    /// left behind, and queue those reviews again.
    pub async fn recover(&self, daemon: &dyn Daemon) -> Result<()> {
        let unreconciled = self.remove_interrupted_runs(daemon).await;
        let kept = match &unreconciled {
            Ok(kept) => kept.clone(),
            Err(_) => self.unlisted_runs(daemon).await?,
        };
        for run in &kept {
            self.hold_unreconciled_run(daemon, run);
        }
        let ids: Vec<ReviewId> = kept.iter().map(|run| run.id).collect();
        let (records, now) = (self.records.clone(), UnixTime::now());
        let (failed, requeued) = self
            .store
            .write(move |tx| store::settle_interrupted(tx, &*records, &ids, now))
            .await?;
        if requeued > 0 {
            log!("queued {requeued} interrupted reviews again");
        }
        // A review that had ended was delivered then.
        for id in failed {
            self.deliver(daemon, id).await;
        }
        unreconciled.map(drop)
    }

    /// The runs of the running reviews, any of whose processes may still
    /// run, for when the run directories cannot be listed.
    async fn unlisted_runs(&self, daemon: &dyn Daemon) -> Result<Vec<Unreconciled>> {
        let mut unlisted = Vec::new();
        for id in self.store.run(|db| store::running(db)).await? {
            unlisted.push(Unreconciled {
                id,
                left_running: true,
                run: self.running_run(daemon, id).await,
            });
        }
        Ok(unlisted)
    }

    /// Where running review `id`'s run is: in the reviewed workspace's
    /// repository's directory, while the workspace exists.
    async fn running_run(&self, daemon: &dyn Daemon, id: ReviewId) -> Option<LeftRun> {
        let review = self.load(id).await.ok()??;
        let workspace = daemon.workspace_by_id(&review.workspace_id).await.ok()?;
        let repository = daemon.repository(workspace.repository()).await.ok()?;
        let place = run_files(&repository, &review).ok()?;
        let beside = run_beside(&place, id);
        Some(LeftRun {
            files: match beside.directory.exists() {
                true => beside,
                false => place,
            },
            repository: repository.id,
        })
    }

    /// Hold what `run`, which could not be removed, may still use until the
    /// daemon restarts: its checkout of the repository and, when its
    /// processes may still run, its tracking and its admission there.
    fn hold_unreconciled_run(&self, daemon: &dyn Daemon, run: &Unreconciled) {
        let Some(left) = &run.run else {
            return;
        };
        if run.left_running {
            self.runs.track(&left.files.directory);
        }
        // Its place names the repository, as removing the workspace deletes
        // the review.
        let repository = &left.repository;
        (self.hold_checkout(run.id, repository, &left.files.directory)).keep();
        if run.left_running {
            daemon.hold_admission(repository);
        }
    }

    /// Remove the runs a stopped daemon left, returning those that could
    /// not be removed; their checkouts stay for the next start.
    async fn remove_interrupted_runs(&self, daemon: &dyn Daemon) -> Result<Vec<Unreconciled>> {
        let mut kept = Vec::new();
        for left in left_runs(daemon).await? {
            // Read before a removal that fails partway can delete its file.
            let review = left.files.review();
            let left_running = self.remove_interrupted_run(&left.files).await;
            if let (Some(id), Some(left_running)) = (review, left_running) {
                kept.push(Unreconciled {
                    id,
                    left_running,
                    run: Some(left),
                });
            }
        }
        Ok(kept)
    }

    /// Remove a run a stopped daemon left, after stopping its reviewer's
    /// processes; when it stays, whether they may still run.
    async fn remove_interrupted_run(&self, files: &Files) -> Option<bool> {
        if let Err(error) = self.runs.stop_leftovers(&files.directory).await {
            log!("{}: {error:#}", files.directory.display());
            return Some(true);
        }
        let error = remove_run(files).await.err()?;
        log!("{}: {error:#}", files.directory.display());
        Some(false)
    }

    /// Whether a run is left in the repository `id`'s directory, as one
    /// whose reviewer's processes may still run: a worktree of the
    /// repository whose link a move would break.
    pub(crate) async fn runs_left(&self, daemon: &dyn Daemon, id: &RepositoryId) -> Result<bool> {
        let repository = daemon.repository(id).await?;
        let dir = review_runs(&repository);
        tokio::task::spawn_blocking(move || {
            Ok(dir.check_own()? && !run_directories(dir.root())?.is_empty())
        })
        .await?
    }
}

/// Whether `agent` can resume a session, through its tool's `review_resume`
/// template.
pub(crate) fn resumes(settings: &Effective, agent: &Agent) -> bool {
    (settings.tools.get(agent.name()))
        .is_some_and(|tool| tool.launch.arguments.review_resume.is_some())
}

/// Review `id`'s run directory with its ID, the output schema `schema` of
/// a run that returns one and a detached checkout of `head`.
async fn prepare_checkout(
    daemon: &dyn Daemon,
    id: ReviewId,
    workspace: &Workspace,
    head: &CommitId,
    files: &Files,
    schema: Option<&str>,
) -> Result<()> {
    let repository = daemon.repository(workspace.repository()).await?;
    review_runs(&repository).create()?;
    std::fs::create_dir_all(&files.directory)
        .with_context(|| format!("create {}", files.directory.display()))?;
    reviewer::write_id(files, id)?;
    if let Some(schema) = schema {
        reviewer::write_schema(files, schema)?;
    }
    git::detached::add(&repository.path, &files.checkout(), head.as_str()).await
}

/// The PR's description, conversation and reviews as its agent account
/// reads them; a review without them still runs.
async fn pull_discussion(
    daemon: &dyn Daemon,
    workspace: &Workspace,
    url: &crate::forge::items::reference::ItemRef,
) -> Option<crate::forge::activity::view::Details> {
    let (forge, account) = match daemon.forge_account(workspace).await {
        Ok(found) => found,
        Err(error) => {
            log!("the discussion of {url}: {error:#}");
            return None;
        }
    };
    let view = forge
        .view(&account, crate::forge::items::link::ItemKind::Pr, url, true)
        .await;
    for error in &view.errors {
        log!("the discussion of {url}: {error}");
    }
    view.details
}

/// The commits the review covers: the workspace's committed HEAD against
/// `shoal diff`'s base, or the PR's fetched head against its base.
pub(crate) async fn review_commits(
    daemon: &dyn Daemon,
    workspace: &Workspace,
    target: &ReviewTarget,
) -> Result<Commits> {
    match target {
        ReviewTarget::Workspace => {
            let head = daemon.head(workspace).await?;
            let base = daemon.diff_base(&workspace.id).await?;
            Ok(Commits { head, base })
        }
        ReviewTarget::Pr { head, base, .. } => {
            let base_ref = git::remote_ref("origin", base.as_str());
            let head = pull_review_head(workspace, head).await?;
            let base = git::diff::merge_base_of(&workspace.path, &head, &base_ref)
                .await
                .with_context(|| format!("the PR base {base} is not fetched"))?;
            Ok(Commits {
                head: head.into(),
                base,
            })
        }
    }
}

/// The commit a PR review reads: the fetched head branch `head`, or for a
/// workspace at a commit, opened at the head of a PR whose branch origin
/// does not have, as a fork's, the head it was last moved to, whatever was
/// committed on top of it there since.
async fn pull_review_head(workspace: &Workspace, head: &BranchName) -> Result<String> {
    if let Some(at) = workspace.at() {
        return Ok(at.commit.clone().into());
    }
    let head_ref = git::remote_ref("origin", head.as_str());
    git::resolve_commit(&workspace.path, &head_ref, git::run)
        .await
        .map(String::from)
        .with_context(|| format!("the PR head {head} is not fetched"))
}

/// The runs stopped daemons may have left, in each repository's directory.
async fn left_runs(daemon: &dyn Daemon) -> Result<Vec<LeftRun>> {
    let mut runs = Vec::new();
    for repository in daemon.repositories().await? {
        let dir = review_runs(&repository);
        if !dir.check_own()? {
            continue;
        }
        runs.extend(
            run_directories(dir.root())?
                .into_iter()
                .map(|files| LeftRun {
                    files,
                    repository: repository.id.clone(),
                }),
        );
    }
    Ok(runs)
}

/// A review's held run, and whether it is at its subject's place, where
/// the subject's session is found.
pub(crate) struct HeldRun<'a> {
    pub(crate) files: Files,
    pub(crate) checkout: CheckoutHold<'a>,
    at_place: bool,
}

/// A run a stopped daemon may have left, with the repository its checkout
/// belongs to.
#[derive(Clone)]
struct LeftRun {
    files: Files,
    repository: RepositoryId,
}

/// A run a stopped daemon left that recovery could not remove.
#[derive(Clone)]
struct Unreconciled {
    id: ReviewId,
    /// Whether its reviewer's processes may still run.
    left_running: bool,
    /// Where it is, when known.
    run: Option<LeftRun>,
}

/// Remove review `id`'s run and give back its checkout and admission. A run whose
/// reviewer `left_running` processes keeps all three, as does a run that
/// could not be removed its checkout, until a restarted daemon removes them.
pub(crate) async fn release_run(
    id: ReviewId,
    files: &Files,
    checkout: CheckoutHold<'_>,
    admission: Box<dyn Admission + '_>,
    left_running: bool,
) {
    if left_running {
        log!("review {id}: its reviewer's processes could not be stopped");
        checkout.keep();
        admission.keep();
        return;
    }
    if let Err(error) = remove_run(files).await {
        log!("review {id}: {error:#}");
        checkout.keep();
    }
}

/// The reviewer `agent` with `model`, asked for an `O` under `scope`: its
/// template's arguments for this run, or its resume template's when it
/// resumes `session`, after its launcher and the arguments selecting the
/// model.
fn reviewer_for<'a, O: reviewer::Output>(
    run: &Run<'a>,
    agent: &Agent,
    model: &ModelChoice,
    session: Option<&AgentSessionId>,
    paths: &shoal_protocol::paths::Paths,
    scope: &RunScope<'_>,
) -> Result<Reviewer<'a>> {
    let Run {
        runs,
        settings,
        configured,
        instructions,
        files,
        ..
    } = run;
    // Config read at run time may have turned the reviewer off since.
    settings.check_reviewer(agent)?;
    let tool = settings
        .tools
        .get(agent.name())
        .with_context(|| format!("no AI tool runs {agent}"))?;
    let arguments = &tool.launch.arguments;
    let prompt = reviewer_prompt::<O>(instructions, session);
    let (schema, output) = (files.schema(), files.output());
    let fields = crate::ai::ReviewFields {
        prompt: &prompt,
        schema: (O::SCHEMA).map(|json| crate::ai::Schema {
            file: &schema,
            json,
        }),
        output: &output,
    };
    let (args, writes_output) = O::arguments(arguments, &fields, session)
        .with_context(|| format!("{agent} has no template for this run"))?;
    let mut environment = configured.clone();
    environment.extend(scope.environment(paths));
    let home = &paths.home;
    Ok(Reviewer {
        runs,
        command: oneshot::agent_command(settings, agent, model, args, &environment, home)?,
        files,
        timeout: reviewer::TIMEOUT,
        writes_output,
        format: tool.review_result,
    })
}

/// What a reviewer is asked for an `O`: `instructions`, after what a
/// resumed session is told when it resumes `session`.
fn reviewer_prompt<O: reviewer::Output>(
    instructions: &str,
    session: Option<&AgentSessionId>,
) -> String {
    match session {
        Some(_) => format!("{}\n\n{instructions}", O::RESUMED),
        None => instructions.to_owned(),
    }
}

/// The runs a stopped daemon left in `directory`.
fn run_directories(directory: &Path) -> Result<Vec<Files>> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("read {}", directory.display())),
    };
    let mut runs = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            runs.push(Files {
                directory: entry.path(),
            });
        }
    }
    Ok(runs)
}

/// The files of `review`'s run, in the reviewed repository's directory, at
/// one place for each workspace and each PR reviewed in it, so its reviewers
/// run in the same directory each time, where an AI tool finds the sessions
/// it saved there.
pub(crate) fn run_files(repository: &Repository, review: &Review) -> Result<Files> {
    let workspace = review.workspace_id.as_str();
    let place = match &review.target {
        ReviewTarget::Workspace => workspace.to_owned(),
        ReviewTarget::Pr { url, .. } => format!("{workspace}-pr-{}", url.pull()?),
    };
    Ok(Files {
        directory: review_runs(repository).root().join(place),
    })
}

/// The files of review `id`'s run beside `place`, its subject's, which a
/// run that could not be removed keeps.
fn run_beside(place: &Files, id: ReviewId) -> Files {
    let mut name = place.directory.file_name().unwrap_or_default().to_owned();
    name.push(format!("-{id}"));
    Files {
        directory: place.directory.with_file_name(name),
    }
}

/// The directory of the repository's review runs.
fn review_runs(repository: &Repository) -> ShoalDir {
    ShoalDir::of(repository, Contents::ReviewRuns)
}

/// Remove a run's checkout and files.
async fn remove_run(files: &Files) -> Result<()> {
    git::detached::remove(&files.checkout()).await?;
    match std::fs::remove_dir_all(&files.directory) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(error).with_context(|| format!("remove {}", files.directory.display()))
        }
        _ => Ok(()),
    }
}
