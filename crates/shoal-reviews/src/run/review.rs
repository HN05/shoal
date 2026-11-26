//! One queued review's passes: its admission, its run in its subject's place
//! and how it ended.
use std::sync::Arc;

use anyhow::Result;
use shoal_protocol::time::UnixTime;
use tokio::sync::watch;

use super::{
    attempts::{InPlace, Task},
    commits::{Commits, review_commits},
    directories::{release_run, run_files},
};
use crate::{
    Reviews,
    agent::Agent,
    daemon::{Admission, Daemon},
    false_positives, log,
    model::Workspace,
    reviewer::Ended,
    reviews::{FailureReason, Review, ReviewId, ReviewState},
    store,
};

/// How one pass of a review ended.
enum Pass {
    Ended,
    /// Protection stopped its run and queued the review again.
    Stopped,
    /// The machine's sleep stopped its run and queued the review again.
    Slept,
}

impl Reviews {
    /// Run queued review `id` to its end. A failure to prepare it fails it,
    /// as the reviewer could not run.
    pub(super) async fn run_review(&self, daemon: &dyn Daemon, id: ReviewId) {
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
            task: Task::Review,
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

    /// Record that review `id` started on `commits`; false when it is no
    /// longer queued.
    async fn start_review(&self, id: ReviewId, commits: &Commits) -> Result<bool> {
        let (head, base, now) = (commits.head.clone(), commits.base.clone(), UnixTime::now());
        self.store
            .run(move |db| store::start(db, id, &head, &base, now))
            .await
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
        ended: Ended,
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
}
