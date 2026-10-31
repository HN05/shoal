//! Running queued reviews: each starts at once, takes the workspace's HEAD,
//! runs its reviewer in a detached checkout, falls back to a second
//! reviewer once, and records what it found.
use std::{collections::HashSet, path::Path, sync::Arc};

use anyhow::{Context, Result};
use tokio::{sync::watch, task::JoinSet};

use super::{
    history, now,
    reviewer::{self, Ended, Files, Reviewer},
    store,
};
use crate::{
    config::{Effective, templates},
    daemon::{log, workspace::Manager},
    model::Workspace,
    reviews::{FailureReason, Review, ReviewState, ReviewTarget},
};

/// Start every queued review, now and whenever one is queued. Reviews take no
/// agent slot, so nothing else limits how many run.
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

/// The commits a run reviews.
struct Commits {
    head: String,
    base: String,
}

impl Manager {
    /// Run queued review `id` to its end. A failure to prepare it fails it,
    /// as the reviewer could not run.
    async fn run_review(&self, id: i64) {
        let (cancel, mut cancelled) = watch::channel(false);
        self.reviews.cancels.lock().unwrap().insert(id, cancel);
        if let Err(error) = self.review_once(id, &mut cancelled).await {
            log!("review {id}: {error:#}");
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
        self.reviews.cancels.lock().unwrap().remove(&id);
        self.reviews.ended.send_modify(|ended| *ended += 1);
    }

    async fn review_once(&self, id: i64, cancelled: &mut watch::Receiver<bool>) -> Result<()> {
        let Some(review) = self.store.run(move |db| store::load(db, id)).await? else {
            return Ok(());
        };
        if review.state != ReviewState::Queued {
            return Ok(());
        }
        let workspace = self.workspace(&review.workspace_id).await?;
        let settings = self.workspace_settings_with_templates(&workspace).await?;
        let commits = self.review_commits(&workspace, &review.target).await?;
        let (head, base, now) = (commits.head.clone(), commits.base.clone(), now());
        if !self
            .store
            .run(move |db| store::start(db, id, &head, &base, now))
            .await?
        {
            return Ok(());
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
        self.record_end(id, &reviewer, ended).await
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
        let repository = self.repository(workspace.repository_id.as_str()).await?;
        std::fs::create_dir_all(&files.directory)
            .with_context(|| format!("create {}", files.directory.display()))?;
        reviewer::write_schema(files)?;
        crate::git::detached::add(&repository.path, &files.checkout(), &commits.head).await?;
        let configured = self.configured_workspace_environment(workspace).await?;
        let instructions = self
            .review_instructions(review, workspace, settings, files, commits)
            .await?;
        let agent = review.agent.clone();
        let ended = self
            .attempt(
                settings,
                &agent,
                &configured,
                &instructions,
                files,
                cancelled,
            )
            .await;
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
                let ended = self
                    .attempt(
                        settings,
                        &fallback,
                        &configured,
                        &instructions,
                        files,
                        cancelled,
                    )
                    .await;
                (fallback, ended)
            }
            (ended, _) => (agent, ended),
        })
    }

    /// One reviewer's run of the review, trusted in its tool's config for
    /// the run's length.
    async fn attempt(
        &self,
        settings: &Effective,
        agent: &str,
        configured: &std::collections::BTreeMap<String, std::ffi::OsString>,
        instructions: &str,
        files: &Files,
        cancelled: &mut watch::Receiver<bool>,
    ) -> Ended {
        let Some(reviewer) = reviewer_for(
            settings,
            agent,
            configured,
            instructions,
            files,
            &self.paths.home,
        ) else {
            log!("{agent} has no review template or launcher");
            return Ended::Failed(FailureReason::ExecutionFailed);
        };
        let trusted = trust(
            settings,
            agent,
            &reviewer,
            &self.paths.home,
            &files.checkout(),
        );
        let ended = reviewer::run(&reviewer, cancelled).await;
        if let Some((entries, config)) = trusted
            && let Err(error) = entries.remove(&config, &[canonical(&files.checkout())])
        {
            log!("{} trust entry of a review: {error:#}", entries.tool);
        }
        let _ = std::fs::remove_file(files.output());
        ended
    }

    /// Record how the run ended and deliver the review; a cancel already
    /// recorded and delivered it.
    async fn record_end(&self, id: i64, reviewer: &str, ended: Ended) -> Result<()> {
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
        if recorded {
            self.deliver_review(id).await;
        }
        Ok(())
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
        let history = history::history(&earlier, pull.as_ref());
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
        let requeued = self.store.run(|db| store::requeue_running(db)).await?;
        if requeued > 0 {
            log!("queued {requeued} interrupted reviews again");
        }
        let directory = self.paths.reviews();
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(error).with_context(|| format!("read {}", directory.display()));
            }
        };
        for entry in entries {
            let files = Files {
                directory: entry?.path(),
            };
            forget_trust(&canonical(&files.checkout()), &self.paths.home);
            remove_run(&files).await?;
        }
        Ok(())
    }
}

/// The reviewer `agent` with the review's fields, if it has a launcher and
/// a review template.
fn reviewer_for<'a>(
    settings: &Effective,
    agent: &str,
    configured: &std::collections::BTreeMap<String, std::ffi::OsString>,
    instructions: &str,
    files: &'a Files,
    home: &Path,
) -> Option<Reviewer<'a>> {
    let tool = settings.tools.get(agent)?;
    let launcher = settings.launchers.get(agent)?;
    let arguments = &tool.launch.arguments;
    let args = arguments.review_args(&crate::ai::ReviewFields {
        prompt: instructions,
        schema: &files.schema(),
        schema_json: crate::reviews::output::SCHEMA,
        output: &files.output(),
    })?;
    let mut environment = configured.clone();
    tool.launch.environment.layer(&mut environment, home);
    Some(Reviewer {
        argv: crate::ai::review_command(launcher, args),
        environment,
        remove: tool.launch.environment.remove.clone(),
        files,
        writes_output: arguments.review_writes_output(),
        format: tool.review_result,
    })
}

/// Mark the checkout trusted in the config the reviewer's tool reads, as
/// workspaces are, so it reads the project's settings; returns the entry to
/// remove afterwards. Development builds leave user files alone.
fn trust(
    settings: &Effective,
    agent: &str,
    reviewer: &Reviewer,
    home: &Path,
    checkout: &Path,
) -> Option<(
    &'static crate::agent::trust::TrustConfig,
    std::path::PathBuf,
)> {
    if !crate::build::changes_user_files() {
        return None;
    }
    let entries = settings.tools.get(agent)?.trust?;
    let added = entries
        .config(home, &reviewer.environment)
        .and_then(|config| entries.add(&config, checkout).map(|_| config));
    match added {
        Ok(config) => Some((entries, config)),
        Err(error) => {
            log!(
                "could not trust a review checkout for {}: {error:#}",
                entries.tool
            );
            None
        }
    }
}

/// Remove trust entries an interrupted review may have left in the tools'
/// default configs.
fn forget_trust(checkout: &Path, home: &Path) {
    if !crate::build::changes_user_files() {
        return;
    }
    let empty = Default::default();
    for entries in [
        &crate::agent::trust::CLAUDE_CODE,
        &crate::agent::trust::CODEX,
    ] {
        let removed = entries
            .config(home, &empty)
            .and_then(|config| entries.remove(&config, &[checkout.to_owned()]));
        if let Err(error) = removed {
            log!("{} trust entry of a review: {error:#}", entries.tool);
        }
    }
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

fn canonical(path: &Path) -> std::path::PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned())
}
