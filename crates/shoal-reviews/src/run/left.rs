//! Runs a stopped daemon left: removing them, or holding what they still use
//! until a restart, and queueing their reviews again.
use anyhow::Result;
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::time::UnixTime;

use super::directories::{remove_run, review_runs, run_beside, run_directories, run_files};
use crate::{Reviews, daemon::Daemon, log, reviewer::Files, reviews::ReviewId, store};

impl Reviews {
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

/// The runs stopped daemons may have left, in each repository's directory.
pub(super) async fn left_runs(daemon: &dyn Daemon) -> Result<Vec<LeftRun>> {
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

/// A run a stopped daemon may have left, with the repository its checkout
/// belongs to.
#[derive(Clone)]
pub(super) struct LeftRun {
    files: Files,
    repository: RepositoryId,
}

/// A run a stopped daemon left that recovery could not remove.
#[derive(Clone)]
pub(super) struct Unreconciled {
    id: ReviewId,
    /// Whether its reviewer's processes may still run.
    left_running: bool,
    /// Where it is, when known.
    run: Option<LeftRun>,
}
