//! The reviewer runs of a review or an answer: each agent's attempt, the
//! fallback agent's and the session they resume.
use anyhow::{Context, Result};
use shoal_daemon_base::callers::Access;
use shoal_ids::agent_session_ids::AgentSessionId;
use tokio::sync::watch;

use super::{
    commits::{Commits, prepare_checkout},
    directories::HeldRun,
};
use crate::{
    Reviews,
    agent::{Agent, ModelChoice},
    config::Effective,
    daemon::{AwaitedPost, Daemon},
    log,
    model::Workspace,
    oneshot,
    reviewer::{self, Ended, Files, Reviewer},
    reviews::{FailureReason, Review, ReviewId, ReviewState},
    scope::RunScope,
    store,
};

/// What a reviewer's run is asked for: writing its review, or answering the
/// holder's post that a finding's thread waits on.
#[derive(Clone, Copy)]
pub(crate) enum Task {
    Review,
    Answer(AwaitedPost),
}

impl Task {
    /// What the scope of a run for review `review` lets it change.
    fn access(self, review: ReviewId) -> Access {
        match self {
            Self::Review => Access::Review(review),
            Self::Answer(awaited) => Access::Answer(awaited.thread),
        }
    }

    /// What a session the run resumes is told first.
    fn resumed(self) -> &'static str {
        match self {
            Self::Review => {
                "Earlier in this session you reviewed an earlier commit of this work. Review the \
                 commit now checked out as the instructions below say; what you learned before \
                 is context, and the history after them says how each earlier finding was \
                 resolved."
            }
            Self::Answer(_) => {
                "Earlier in this session you reviewed this work. Answer as the instructions \
                 below say; what you learned before is context."
            }
        }
    }
}

/// A run in the place of a review's subject: what it is asked for, the
/// review, its workspace and settings, the commits it reads, and the run
/// it holds there.
pub(crate) struct InPlace<'a> {
    pub(crate) task: Task,
    pub(crate) review: &'a Review,
    pub(crate) workspace: &'a Workspace,
    pub(crate) settings: &'a Effective,
    pub(crate) commits: &'a Commits,
    pub(crate) held: &'a HeldRun<'a>,
}

/// One agent's attempt at a run, for a review.
pub(super) struct Attempt<'a> {
    run: &'a Run<'a>,
    review: &'a Review,
    agent: &'a Agent,
}

/// What every reviewer of a run gets.
pub(super) struct Run<'a> {
    task: Task,
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

impl Reviews {
    /// Check the reviewed commit out in the run `place` holds and run the
    /// reviewers there, continuing the subject's session; returns the agent
    /// that produced the review and how it ended.
    pub(super) async fn run_in_checkout(
        &self,
        daemon: &dyn Daemon,
        place: &InPlace<'_>,
        cancelled: &mut watch::Receiver<bool>,
    ) -> Result<(Agent, Ended)> {
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
    pub(crate) async fn run_reviewer(
        &self,
        daemon: &dyn Daemon,
        place: &InPlace<'_>,
        agent: &Agent,
        instructions: String,
        session: Option<AgentSessionId>,
        cancelled: &mut watch::Receiver<bool>,
    ) -> Result<(Agent, Ended)> {
        let InPlace {
            task,
            review,
            workspace,
            settings,
            commits,
            held,
        } = place;
        let files = &held.files;
        prepare_checkout(daemon, review.id, workspace, &commits.head, files).await?;
        let run = Run {
            task: *task,
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
    async fn review_with_fallback(
        &self,
        attempt: &Attempt<'_>,
        session: Option<AgentSessionId>,
        cancelled: &mut watch::Receiver<bool>,
    ) -> (Agent, Ended) {
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
    async fn review_by_agent(
        &self,
        run: &Run<'_>,
        id: ReviewId,
        agent: &Agent,
        model: &ModelChoice,
        session: Option<AgentSessionId>,
        cancelled: &mut watch::Receiver<bool>,
    ) -> Ended {
        if let Some(session) = session {
            match self
                .attempt(run, id, agent, model, Some(&session), cancelled)
                .await
            {
                Ended::Returned { session: reported } => {
                    let session = reported.or(Some(session));
                    return Ended::Returned { session };
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
    async fn attempt(
        &self,
        run: &Run<'_>,
        id: ReviewId,
        agent: &Agent,
        model: &ModelChoice,
        session: Option<&AgentSessionId>,
        cancelled: &mut watch::Receiver<bool>,
    ) -> Ended {
        self.record_attempt(id, agent).await;
        let scope = RunScope::of(&self.operations, &run.workspace.id, run.task.access(id));
        let reviewer = reviewer_for(run, agent, model, session, &self.paths, &scope);
        let ended = match reviewer {
            Ok(reviewer) => reviewer::run(&reviewer, cancelled).await,
            Err(error) => {
                log!("review by {agent}: {error:#}");
                Ended::Failed(FailureReason::ExecutionFailed)
            }
        };
        drop(scope);
        let ended = self.settle(run.task, id, ended).await;
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

    /// How the run for `task` of review `id`, which ended as `ended`,
    /// settled: as [`reviewer::settled`] tells from whether it produced the
    /// review it submitted or the answer it posted; cancelled for an answer
    /// that failed once its post no longer waits, which no other run could
    /// give.
    async fn settle(&self, task: Task, id: ReviewId, ended: Ended) -> Ended {
        let awaited = match task {
            Task::Review => return reviewer::settled(ended, self.finished(id).await),
            Task::Answer(awaited) => awaited,
        };
        match reviewer::settled(ended, self.posted_answer(awaited).await) {
            Ended::Failed(_) if !self.awaits(awaited).await => Ended::Cancelled,
            ended => ended,
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
}

/// Whether `agent` can resume a session, through its tool's `review_resume`
/// template.
pub(crate) fn resumes(settings: &Effective, agent: &Agent) -> bool {
    (settings.tools.get(agent.name()))
        .is_some_and(|tool| tool.launch.arguments.review_resume.is_some())
}

/// The reviewer `agent` with `model`, asked for `run`'s task under `scope`:
/// its review template's arguments, or its resume template's when it
/// resumes `session`, after its launcher and the arguments selecting the
/// model.
pub(super) fn reviewer_for<'a>(
    run: &Run<'a>,
    agent: &Agent,
    model: &ModelChoice,
    session: Option<&AgentSessionId>,
    paths: &shoal_protocol::paths::Paths,
    scope: &RunScope<'_>,
) -> Result<Reviewer<'a>> {
    let Run {
        task,
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
    let prompt = reviewer_prompt(*task, instructions, session);
    let output = files.output();
    let fields = crate::ai::ReviewFields {
        prompt: &prompt,
        schema: None,
        output: &output,
    };
    let (args, writes_output) = match session {
        Some(session) => (
            arguments.review_resume_args(&fields, session),
            arguments.review_resume_writes_output(),
        ),
        None => (
            arguments.review_args(&fields),
            arguments.review_writes_output(),
        ),
    };
    let args = args.with_context(|| format!("{agent} has no template for this run"))?;
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

/// What a reviewer is asked for `task`: `instructions`, after what a
/// resumed session is told when it resumes `session`.
pub(super) fn reviewer_prompt(
    task: Task,
    instructions: &str,
    session: Option<&AgentSessionId>,
) -> String {
    match session {
        Some(_) => format!("{}\n\n{instructions}", task.resumed()),
        None => instructions.to_owned(),
    }
}
