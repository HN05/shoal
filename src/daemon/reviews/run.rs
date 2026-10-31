//! Running queued reviews: each starts at once, takes the workspace's HEAD,
//! runs its reviewer in a detached checkout, falls back to a second
//! reviewer once, and records what it found.
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result};
use tokio::{sync::watch, task::JoinSet};

use super::{
    history, now,
    reviewer::{self, Ended, Files, Reviewer},
    store,
};
use crate::{
    agent::ModelChoice,
    config::{Effective, templates},
    daemon::{log, oneshot, workspace::Manager},
    model::Workspace,
    reviews::{FailureReason, Review, ReviewState, ReviewTarget},
};

/// Start every queued review, now and whenever one is queued; each then
/// waits in the agent queue for a slot.
pub(crate) async fn run(manager: Arc<Manager>) {
    let mut running = JoinSet::new();
    let mut started = HashSet::new();
    loop {
        let queued = manager.reviews.queued.notified();
        tokio::pin!(queued);
        queued.as_mut().enable();
        match manager.store.run(|db| store::queued(db)).await {
            Ok(ids) => {
                for id in ids.into_iter().filter(|id| started.insert(*id)) {
                    let manager = manager.clone();
                    running.spawn(async move {
                        manager.run_review(id).await;
                        id
                    });
                }
            }
            Err(error) => log!("queued reviews: {error:#}"),
        }
        tokio::select! {
            () = &mut queued => {}
            Some(ended) = running.join_next(), if !running.is_empty() => {
                if let Ok(id) = ended {
                    started.remove(&id);
                }
            }
        }
    }
}

/// How one pass of a review ended.
enum Pass {
    Ended,
    /// Protection stopped its run and queued the review again.
    Stopped,
}

/// What every reviewer of a run gets.
struct Run<'a> {
    settings: &'a Effective,
    /// The workspace's configured variables.
    configured: std::collections::BTreeMap<String, std::ffi::OsString>,
    instructions: String,
    files: &'a Files,
}

/// The commits a run reviews.
struct Commits {
    head: String,
    base: String,
}

impl Manager {
    /// Run queued review `id` to its end. A failure to prepare it fails it,
    /// as the reviewer could not run.
    async fn run_review(&self, id: i64) {
        let mut after_stop = false;
        loop {
            let (cancel, mut cancelled) = watch::channel(false);
            let cancel = Arc::new(cancel);
            self.reviews
                .cancels
                .lock()
                .unwrap()
                .insert(id, cancel.clone());
            match self
                .review_once(id, &cancel, &mut cancelled, after_stop)
                .await
            {
                Ok(Pass::Stopped) => {
                    log!("review {id}: protection stopped it; it runs again once load recovers");
                    after_stop = true;
                }
                Ok(Pass::Ended) => break,
                Err(error) => {
                    self.fail_unprepared(id, error).await;
                    break;
                }
            }
        }
        self.reviews.cancels.lock().unwrap().remove(&id);
        self.reviews.ended.send_modify(|ended| *ended += 1);
    }

    /// Fail review `id`, which could not be prepared, as its reviewer could
    /// not run.
    async fn fail_unprepared(&self, id: i64, error: anyhow::Error) {
        log!("review {id}: {error:#}");
        let _operation = self.gates.operation().await;
        let now = now();
        let failed = self
            .store
            .run(move |db| store::fail(db, id, None, FailureReason::ExecutionFailed, now))
            .await;
        match failed {
            Ok(true) => self.deliver_review(id).await,
            Ok(false) => {}
            Err(error) => log!("review {id}: {error:#}"),
        }
    }

    /// Run queued review `id` once it has an agent slot, which protection
    /// stops by sending `cancel`; a run it stopped asks for one `after_stop`.
    async fn review_once(
        &self,
        id: i64,
        cancel: &Arc<watch::Sender<bool>>,
        cancelled: &mut watch::Receiver<bool>,
        after_stop: bool,
    ) -> Result<Pass> {
        let Some(review) = self.store.run(move |db| store::load(db, id)).await? else {
            return Ok(Pass::Ended);
        };
        if review.state != ReviewState::Queued {
            return Ok(Pass::Ended);
        }
        let workspace = self.workspace(&review.workspace_id).await?;
        let stop = cancel.clone();
        let mut slot = self.agent_queue.request_oneshot(
            workspace.repository_id.as_str(),
            after_stop,
            move || {
                stop.send_replace(true);
            },
        );
        tokio::select! {
            () = slot.admitted() => {}
            // A cancel or the workspace's removal recorded the review's end.
            _ = cancelled.wait_for(|cancelled| *cancelled) => return Ok(Pass::Ended),
        }
        // Its configuration and commits are read only from the worktree
        // that is this workspace's.
        self.verify_worktree(&workspace).await?;
        let settings = self.workspace_settings_with_templates(&workspace).await?;
        let commits = self.review_commits(&workspace, &review.target).await?;
        let (head, base, now) = (commits.head.clone(), commits.base.clone(), now());
        if !self
            .store
            .run(move |db| store::start(db, id, &head, &base, now))
            .await?
        {
            return Ok(Pass::Ended);
        }
        let files = Files {
            directory: self.paths.reviews().join(id.to_string()),
        };
        let ended = self
            .run_in_checkout(&review, &workspace, &settings, &commits, &files, cancelled)
            .await;
        if let Err(error) = remove_run(&files).await {
            log!("review {id}: {error:#}");
        }
        let (reviewer, ended) = ended?;
        self.record_end(id, &workspace, &reviewer, ended).await
    }

    /// Check the reviewed commit out and run the reviewers there; returns
    /// the agent that produced the result and how it ended.
    async fn run_in_checkout(
        &self,
        review: &Review,
        workspace: &Workspace,
        settings: &Effective,
        commits: &Commits,
        files: &Files,
        cancelled: &mut watch::Receiver<bool>,
    ) -> Result<(String, Ended)> {
        self.prepare_checkout(workspace, &commits.head, files)
            .await?;
        let run = Run {
            settings,
            configured: self
                .configured_environment_from(workspace, settings)
                .await?,
            instructions: self
                .review_instructions(review, workspace, settings, files, commits)
                .await?,
            files,
        };
        let agent = review.agent.clone();
        let model = settings.model(&agent, review.model.clone());
        let ended = self.attempt(&run, &agent, &model, cancelled).await;
        let fallback = settings
            .review
            .fallback_agent
            .as_ref()
            .map(|agent| agent.name().to_owned())
            .filter(|fallback| *fallback != agent);
        Ok(match (ended, fallback) {
            (Ended::Failed(reason), Some(fallback)) if reason != FailureReason::TimedOut => {
                log!(
                    "review {}: {agent} failed ({reason}); {fallback} reviews instead",
                    review.id
                );
                let model = settings.model(&fallback, Default::default());
                let ended = self.attempt(&run, &fallback, &model, cancelled).await;
                (fallback, ended)
            }
            (ended, _) => (agent, ended),
        })
    }

    /// The run's directory with the output schema and a detached checkout
    /// of `head`.
    async fn prepare_checkout(
        &self,
        workspace: &Workspace,
        head: &str,
        files: &Files,
    ) -> Result<()> {
        let repository = self.repository(workspace.repository_id.as_str()).await?;
        std::fs::create_dir_all(&files.directory)
            .with_context(|| format!("create {}", files.directory.display()))?;
        reviewer::write_schema(files)?;
        crate::git::detached::add(&repository.path, &files.checkout(), head).await
    }

    /// One reviewer's attempt at `run`: agent `agent` with `model`.
    async fn attempt(
        &self,
        run: &Run<'_>,
        agent: &str,
        model: &ModelChoice,
        cancelled: &mut watch::Receiver<bool>,
    ) -> Ended {
        let reviewer = match reviewer_for(run, agent, model, &self.paths.home) {
            Ok(reviewer) => reviewer,
            Err(error) => {
                log!("review by {agent}: {error:#}");
                return Ended::Failed(FailureReason::ExecutionFailed);
            }
        };
        reviewer::run(&reviewer, cancelled).await
    }

    /// Record how the run ended, post a finished PR review and deliver the
    /// review, under an operation guard. A cancel already recorded and
    /// delivered it; a run that ended cancelled while still running was
    /// stopped by protection, and its review is queued again.
    async fn record_end(
        &self,
        id: i64,
        workspace: &Workspace,
        reviewer: &str,
        ended: Ended,
    ) -> Result<Pass> {
        // A managed update waits for the review's posting and delivery,
        // which a restart would not redo once the result is recorded.
        let _operation = self.gates.operation().await;
        if ended == Ended::Cancelled {
            let requeued = self.store.run(move |db| store::requeue(db, id)).await?;
            return Ok(if requeued { Pass::Stopped } else { Pass::Ended });
        }
        let reviewer = reviewer.to_owned();
        let now = now();
        let recorded = self
            .store
            .run(move |db| match ended {
                Ended::Returned(output) => store::finish(db, id, &reviewer, &output, now),
                Ended::Failed(reason) => store::fail(db, id, Some(&reviewer), reason, now),
                Ended::Cancelled => Ok(false),
            })
            .await?;
        if !recorded {
            return Ok(Pass::Ended);
        }
        let review = self.store.run(move |db| store::load(db, id)).await?;
        if let Some(review) = review.filter(|review| review.state == ReviewState::Finished) {
            self.post_review(&review, workspace).await;
        }
        self.deliver_review(id).await;
        Ok(Pass::Ended)
    }

    /// The review's instructions for its checkout, then its history.
    async fn review_instructions(
        &self,
        review: &Review,
        workspace: &Workspace,
        settings: &Effective,
        files: &Files,
        commits: &Commits,
    ) -> Result<String> {
        let checkout = files.checkout();
        let instructions = templates::review_instructions(
            Some(&settings.agent.templates.review),
            &templates::ReviewFields {
                workspace: &workspace.name,
                branch: &workspace.branch,
                path: &checkout.to_string_lossy(),
                base: &commits.base,
            },
        );
        let (id, workspace_id) = (review.id, workspace.id.clone());
        let earlier = self
            .store
            .run(move |db| store::earlier(db, &workspace_id, id))
            .await?;
        let pull = match &review.target {
            ReviewTarget::Pr { url, .. } => self.pull_discussion(workspace, url).await,
            ReviewTarget::Workspace => None,
        };
        // The history fills what the instructions leave of the prompt.
        let room = history::PROMPT_LIMIT.saturating_sub(instructions.len() + 2);
        let history = history::history(&earlier, pull.as_ref(), room);
        Ok(match history.is_empty() {
            true => instructions,
            false => format!("{instructions}\n\n{history}"),
        })
    }

    /// The PR's description, conversation and reviews as its agent account
    /// reads them; a review without them still runs.
    async fn pull_discussion(
        &self,
        workspace: &Workspace,
        url: &str,
    ) -> Option<crate::forge::review::view::Details> {
        let (forge, account) = match self.workspace_forge_account(workspace).await {
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
    async fn review_commits(
        &self,
        workspace: &Workspace,
        target: &ReviewTarget,
    ) -> Result<Commits> {
        match target {
            ReviewTarget::Workspace => {
                let head = crate::daemon::links::pr::current_head(workspace).await?;
                let base = self.diff_base(&workspace.id).await?.commit;
                Ok(Commits { head, base })
            }
            ReviewTarget::Pr { head, base, .. } => {
                let head_ref = crate::git::remote_ref("origin", head);
                let base_ref = crate::git::remote_ref("origin", base);
                let head = crate::git::resolve_commit(&workspace.path, &head_ref, crate::git::run)
                    .await
                    .with_context(|| format!("the PR head {head} is not fetched"))?;
                let base = crate::git::diff::merge_base_of(&workspace.path, &head, &base_ref)
                    .await
                    .with_context(|| format!("the PR base {base} is not fetched"))?;
                Ok(Commits { head, base })
            }
        }
    }

    /// Remove the checkouts and output that reviews a daemon stop interrupted
    /// left behind, and queue those reviews again.
    pub(crate) async fn recover_reviews(&self) -> Result<()> {
        let unreconciled = self.remove_interrupted_runs().await;
        let kept = match &unreconciled {
            Ok(kept) => kept.clone(),
            Err(_) => self.store.run(|db| store::running(db)).await?,
        };
        let failed = kept.clone();
        let now = now();
        let requeued = self
            .store
            .run(move |db| {
                for id in &failed {
                    store::fail(db, *id, None, FailureReason::ExecutionFailed, now)?;
                }
                store::requeue_running(db, &failed)
            })
            .await?;
        if requeued > 0 {
            log!("queued {requeued} interrupted reviews again");
        }
        for id in kept {
            self.deliver_review(id).await;
        }
        unreconciled.map(drop)
    }

    /// Remove the runs a stopped daemon left, returning the reviews whose
    /// runs could not be removed; their checkouts stay for the next start.
    async fn remove_interrupted_runs(&self) -> Result<Vec<i64>> {
        let mut kept = Vec::new();
        for files in run_directories(&self.paths.reviews())? {
            if let Err(error) = remove_interrupted_run(&files).await {
                log!("{}: {error:#}", files.directory.display());
                kept.extend(files.review());
            }
        }
        Ok(kept)
    }
}

/// The reviewer `agent` with the review's fields, if it has a launcher and
/// The reviewer `agent` with `model`: its review template's arguments for
/// this run, after its launcher and the arguments selecting the model.
fn reviewer_for<'a>(
    run: &Run<'a>,
    agent: &str,
    model: &ModelChoice,
    home: &Path,
) -> Result<Reviewer<'a>> {
    let Run {
        settings,
        configured,
        instructions,
        files,
    } = run;
    let tool = settings
        .tools
        .get(agent)
        .with_context(|| format!("no AI tool runs {agent}"))?;
    let arguments = &tool.launch.arguments;
    let args = arguments
        .review_args(&crate::ai::ReviewFields {
            prompt: instructions,
            schema: &files.schema(),
            schema_json: crate::reviews::output::SCHEMA,
            output: &files.output(),
        })
        .with_context(|| format!("{agent} has no review template"))?;
    Ok(Reviewer {
        command: oneshot::agent_command(settings, agent, model, args, configured, home)?,
        files,
        writes_output: arguments.review_writes_output(),
        format: tool.review_result,
    })
}

/// The runs a stopped daemon left in `directory`.
fn run_directories(directory: &Path) -> Result<Vec<Files>> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("read {}", directory.display())),
    };
    entries
        .map(|entry| {
            Ok(Files {
                directory: entry?.path(),
            })
        })
        .collect()
}

/// Remove a run a stopped daemon left, after stopping its reviewer's
/// processes still working in its checkout.
async fn remove_interrupted_run(files: &Files) -> Result<()> {
    oneshot::stop_leftovers(&canonical(&files.checkout())).await?;
    remove_run(files).await
}

/// Remove a run's checkout and files.
async fn remove_run(files: &Files) -> Result<()> {
    crate::git::detached::remove(&files.checkout()).await?;
    match std::fs::remove_dir_all(&files.directory) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(error).with_context(|| format!("remove {}", files.directory.display()))
        }
        _ => Ok(()),
    }
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned())
}
