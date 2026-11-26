//! The reviewer's answers to posts in its findings' threads.
use std::sync::Arc;

use anyhow::{Context, Result};
use shoal_daemon_base::callers::{Access, Caller};
use shoal_ids::agent_session_ids::AgentSessionId;
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::threads::{Sender, Thread};
use tokio::sync::watch;

use crate::{
    Reviews,
    agent::Agent,
    config::Effective,
    daemon::{AwaitedPost, Daemon},
    history, log,
    model::Workspace,
    reviewer::{Ended, Files},
    reviews::{FailureReason, Review, ReviewId},
    run::{self, InPlace, Task},
    store,
};

impl Reviews {
    /// Wake the scheduler for a post in a finding's thread, which may now
    /// wait for the reviewer's answer.
    pub fn answer_posts(&self) {
        self.queued.notify_one();
    }

    /// Of the posts in findings' threads that wait for an answer,
    /// `awaited`, those the reviewer has not run for, forgetting those that
    /// no longer wait.
    pub(crate) fn unanswered(&self, awaited: &[AwaitedPost]) -> Vec<AwaitedPost> {
        let mut answered = self.answered.lock().unwrap();
        answered.retain(|post| awaited.contains(post));
        (awaited.iter())
            .filter(|post| !answered.contains(post))
            .copied()
            .collect()
    }

    /// Have the reviewer answer the post `awaited` in its thread, or post
    /// why it gave no answer there, running once for it whatever comes of
    /// it, as a thread too long for another post stays waiting. Its end,
    /// after that post, wakes what waits for the place its run held.
    pub(crate) async fn answer(&self, daemon: &dyn Daemon, awaited: AwaitedPost) {
        self.answered.lock().unwrap().insert(awaited);
        if let Some(unanswered) = self.answer_once(daemon, awaited).await {
            post_why(daemon, unanswered).await;
        }
        self.answering.lock().unwrap().remove(&awaited);
        self.ended.send_modify(|ended| *ended += 1);
    }

    /// Stop the answers being made for the spaces `stopped` selects, as
    /// the removal of their workspace or repository does; the posts whose
    /// answers it stopped.
    pub(crate) fn stop_answers(&self, stopped: impl Fn(&AnswerRun) -> bool) -> Vec<AwaitedPost> {
        let answering = self.answering.lock().unwrap();
        let stopping = answering.iter().filter(|(_, run)| stopped(run));
        (stopping.map(|(post, run)| {
            run.cancel.send_replace(true);
            *post
        }))
        .collect()
    }

    /// Whether an answer to one of `posts` is still being made.
    pub(crate) fn answering_any(&self, posts: &[AwaitedPost]) -> bool {
        let answering = self.answering.lock().unwrap();
        posts.iter().any(|post| answering.contains_key(post))
    }

    /// Have the reviewer answer the post `awaited`; why it gave no answer,
    /// none once it answered or the post no longer waits, or when
    /// protection stopped the run, which runs again for the holder's next
    /// post.
    async fn answer_once(&self, daemon: &dyn Daemon, awaited: AwaitedPost) -> Option<Unanswered> {
        let thread = match self.awaiting(awaited).await {
            Ok(thread) => thread?,
            Err(error) => {
                log!("answering thread {}: {error:#}", awaited.thread);
                return None;
            }
        };
        let why = match self.answer_in_place(daemon, awaited, &thread).await {
            Ok(ended) => unanswered(ended?)?,
            Err(error) => {
                log!("answering thread {}: {error:#}", awaited.thread);
                format!("The reviewer could not answer: {error:#}")
            }
        };
        Some(Unanswered { thread, why })
    }

    /// Run the reviewer on the post `awaited` in `thread` in the place of its
    /// review's subject once the agent queue admits it; how it ended, none
    /// once the post no longer waits.
    async fn answer_in_place(
        &self,
        daemon: &dyn Daemon,
        awaited: AwaitedPost,
        thread: &Thread,
    ) -> Result<Option<Ended>> {
        let review = self.review_of(thread).await?;
        let workspace = daemon.workspace_by_id(&review.workspace_id).await?;
        let (cancel, mut cancelled) = watch::channel(false);
        let cancel = Arc::new(cancel);
        let run = AnswerRun {
            workspace: workspace.id.clone(),
            repository: workspace.repository().clone(),
            cancel: cancel.clone(),
        };
        self.answering.lock().unwrap().insert(awaited, run);
        let admitted = self.await_admission(daemon, review.id, &workspace, &cancel, &mut cancelled);
        let Some(admission) = admitted.await? else {
            return Ok(None);
        };
        // The holder may have resolved the thread or posted again while the
        // answer waited, and the workspace moved, as a fork PR's does.
        let Some(thread) = self.awaiting(awaited).await? else {
            return Ok(None);
        };
        let workspace = daemon.workspace_by_id(&review.workspace_id).await?;
        daemon.verify_worktree(&workspace).await?;
        let settings = daemon.settings_with_templates(&workspace).await?;
        let commits = run::review_commits(daemon, &workspace, &review.target).await?;
        let repository = daemon.repository(workspace.repository()).await?;
        let place = run::run_files(&repository, &review)?;
        let held = self.hold_run(review.id, &repository.id, place).await?;
        let in_place = InPlace {
            task: Task::Answer(awaited),
            review: &review,
            workspace: &workspace,
            settings: &settings,
            commits: &commits,
            held: &held,
        };
        let ended = (self.run_answer(daemon, &in_place, &thread, &mut cancelled)).await;
        let left_running = matches!(ended, Ok(Ended::LeftRunning));
        run::release_run(
            review.id,
            &held.files,
            held.checkout,
            admission,
            left_running,
        )
        .await;
        ended.map(Some)
    }

    /// The review whose finding `thread` is.
    async fn review_of(&self, thread: &Thread) -> Result<Review> {
        let id = (thread.finding.and_then(|finding| finding.review))
            .with_context(|| format!("thread {} names no review", thread.number))?;
        self.stored(id).await
    }

    /// Run the reviewer in `place` on the latest post of `thread`, resuming
    /// the session its subject's reviews continue, which then goes on as
    /// the one it reported.
    async fn run_answer(
        &self,
        daemon: &dyn Daemon,
        place: &InPlace<'_>,
        thread: &Thread,
        cancelled: &mut watch::Receiver<bool>,
    ) -> Result<Ended> {
        let review = place.review;
        let saved = self.answer_session(review, place.settings).await;
        let agent = match &saved {
            Some((_, agent, _)) => agent.clone(),
            None => (review.reviewer.clone())
                .or_else(|| review.agent().cloned())
                .context("only Shoal's reviews answer their findings")?,
        };
        let session = saved.as_ref().map(|(_, _, session)| session.clone());
        let instructions = instructions(place.workspace, &place.held.files, thread);
        let ran = self.run_reviewer(daemon, place, &agent, instructions, session, cancelled);
        let (answered_by, ended) = ran.await?;
        if let (
            Some((holder, ..)),
            Ended::Returned {
                session: Some(reported),
            },
        ) = (saved, &ended)
            && answered_by == agent
        {
            self.continue_session(holder, reported.clone()).await;
        }
        Ok(ended)
    }

    /// The session the reviews of `review`'s subject continue, with the
    /// review that holds it, when its agent can resume it.
    async fn answer_session(
        &self,
        review: &Review,
        settings: &Effective,
    ) -> Option<(ReviewId, Agent, AgentSessionId)> {
        let id = review.id;
        match self
            .store
            .run(move |db| store::latest_session(db, id))
            .await
        {
            Ok(saved) => saved.filter(|(_, agent, _)| run::resumes(settings, agent)),
            Err(error) => {
                log!("review {id}: its reviewer's session: {error:#}");
                None
            }
        }
    }

    /// Record that the session review `id` holds went on as `session`.
    async fn continue_session(&self, id: ReviewId, session: AgentSessionId) {
        let continued = (self.store).run(move |db| store::continue_session(db, id, &session));
        if let Err(error) = continued.await {
            log!("review {id}: its reviewer's session: {error:#}");
        }
    }

    /// Whether Shoal posted in the thread of `awaited` after it, as the
    /// reviewer's answer is.
    pub(crate) async fn posted_answer(&self, awaited: AwaitedPost) -> bool {
        let records = self.records.clone();
        let threads = (self.store).run(move |db| records.threads(db, &[awaited.thread]));
        match threads.await {
            Ok(threads) => (threads.iter().flat_map(|thread| &thread.posts))
                .any(|post| post.id > awaited.post && post.sender == Sender::Shoal),
            Err(error) => {
                log!("answering thread {}: {error:#}", awaited.thread);
                false
            }
        }
    }

    /// Whether the post `awaited` still waits for the reviewer's answer, as
    /// it is taken to when that cannot be read.
    pub(crate) async fn awaits(&self, awaited: AwaitedPost) -> bool {
        match self.awaiting(awaited).await {
            Ok(thread) => thread.is_some(),
            Err(error) => {
                log!("answering thread {}: {error:#}", awaited.thread);
                true
            }
        }
    }

    /// The thread of the post `awaited` while it waits for the reviewer's
    /// answer.
    async fn awaiting(&self, awaited: AwaitedPost) -> Result<Option<Thread>> {
        let records = self.records.clone();
        let threads = (self.store)
            .run(move |db| {
                if !records.awaiting_answers(db)?.contains(&awaited) {
                    return Ok(Vec::new());
                }
                records.threads(db, &[awaited.thread])
            })
            .await?;
        Ok(threads.into_iter().next())
    }
}

/// An answer being made: the workspace and repository it reads, and what
/// stops it.
pub(crate) struct AnswerRun {
    pub(crate) workspace: SpaceId,
    pub(crate) repository: RepositoryId,
    cancel: Arc<watch::Sender<bool>>,
}

/// Why the reviewer gave no answer in a finding's thread, which Shoal
/// posts there.
struct Unanswered {
    thread: Thread,
    why: String,
}

/// Post in its thread why the reviewer gave no answer, as an answer run
/// posts its answer.
async fn post_why(daemon: &dyn Daemon, unanswered: Unanswered) {
    let Unanswered { thread, why } = unanswered;
    let caller = Caller {
        access: Access::Answer(thread.id),
        ..Caller::of_space(thread.space_id.clone())
    };
    if let Err(error) = daemon.post_in_thread(&caller, thread.number, why).await {
        log!("answering thread {}: {error:#}", thread.number);
    }
}

/// What the reviewer is asked: to answer the latest post of `thread`, one
/// of its findings on `workspace`, from the checkout `files` holds.
fn instructions(workspace: &Workspace, files: &Files, thread: &Thread) -> String {
    format!(
        "You are the reviewer of the committed changes of Shoal workspace {}, checked out at {} \
         at its latest commit. You have read-only access: read files, search, and run `git \
         diff`, `git log`, `git show` and `git blame`. Do not edit files, commit or \
         push.\n\nThe thread below is one of your findings with its discussion so far. Its \
         author's latest post answers or asks you something instead of resolving it. Check the \
         code, then post your reply once, with `shoal send {} \"<reply>\"`, in a few short \
         sentences: whether their point holds, and where the finding still stands, what would \
         settle it. Raise no new findings here.\n\n{}",
        workspace.directory_name(),
        files.checkout().display(),
        thread.number,
        history::thread_entry(thread)
    )
}

/// Why the run that ended as `ended` gave no answer; none for one that
/// posted it, or that protection stopped.
fn unanswered(ended: Ended) -> Option<String> {
    match ended {
        Ended::Returned { .. } | Ended::Cancelled => None,
        Ended::Failed(FailureReason::NotSubmitted) => {
            Some("The reviewer exited without answering.".into())
        }
        Ended::Failed(reason) => Some(format!(
            "The reviewer could not answer: {}.",
            reason.describe()
        )),
        Ended::LeftRunning => {
            Some("The reviewer could not answer: its run could not be stopped.".into())
        }
    }
}
