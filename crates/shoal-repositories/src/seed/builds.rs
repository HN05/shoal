//! Keeping each repository's seed at its default branch's tip. A seed
//! failure never fails the operation that met it: a workspace without a
//! seed still builds, only from scratch.
use std::{collections::BTreeMap, ffi::OsString, sync::Arc, time::Duration};

use anyhow::{Result, ensure};
use shoal_core::{
    config::Effective,
    fsutil::off_runtime,
    git::{
        self,
        default_branch::{self, DefaultBranchLookup},
    },
};
use shoal_daemon_base::{
    alerts::NewAlert,
    log,
    notifications::{About, AlertKind},
};
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::model::Repository;
use shoal_refs::{BranchName, CommitId};

use super::{
    Layout, Snapshot, Staged,
    build::{self, Build, Previous, Toolchains},
    steps::{StepFailure, run_build},
    worktree::check_out_seed,
};
use crate::{Repositories, daemon::Daemon};

/// How often the daemon looks for a default branch that moved.
const REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// What one repository's seed builds, from its resolved `[setup.seed]` table.
pub(super) struct Plan {
    branch: BranchName,
    pub(super) build: Build,
    pub(super) paths: Vec<String>,
    pub(super) environment: BTreeMap<String, OsString>,
    caches: BTreeMap<String, shoal_core::config::repo::Cache>,
}

impl Plan {
    /// The seed `settings` describe, or `None` when they configure none.
    pub(super) async fn new(
        repo: &Repository,
        layout: &Layout,
        settings: &Effective,
    ) -> Result<Option<Self>> {
        let presets = settings.setup.presets.as_deref().unwrap_or_default();
        let Some(build) = Build::of(&settings.setup.seed, presets, &layout.worktree()) else {
            return Ok(None);
        };
        let branch = default_branch::resolve(&repo.path, DefaultBranchLookup::Cached).await?;
        // Builds see the workspaces' [env], so their outputs stay reusable.
        Ok(Some(Self {
            build,
            paths: build::outputs(&settings.setup.seed, presets),
            environment: shoal_core::env::render_seed_environment(
                &settings.env,
                &layout.worktree(),
                branch.as_str(),
                repo.display_name(),
                &shoal_core::caches::locate(repo, &settings.caches),
            ),
            caches: settings.caches.clone(),
            branch,
        }))
    }

    /// What a build depends on besides the commit, so a snapshot built under
    /// other settings is rebuilt.
    pub(super) fn settings(&self) -> Result<String> {
        Ok(serde_json::to_string(&(
            &self.build,
            &self.paths,
            &self.environment,
        ))?)
    }
}

pub async fn run(repositories: Arc<Repositories>, daemon: Arc<dyn Daemon>) {
    loop {
        repositories.refresh_seeds(&*daemon).await;
        tokio::time::sleep(REFRESH_INTERVAL).await;
    }
}

/// Refill spares apart from builds, which can run for minutes: when a
/// workspace took one or a build froze a snapshot, and every
/// [`REFRESH_INTERVAL`] after a failure or a daemon restart.
pub async fn run_spares(repositories: Arc<Repositories>, daemon: Arc<dyn Daemon>) {
    loop {
        repositories.refill_spares(&*daemon).await;
        tokio::select! {
            _ = tokio::time::sleep(REFRESH_INTERVAL) => {}
            _ = repositories.seeds.spare_wanted() => {}
        }
    }
}

impl Repositories {
    /// Rebuild, one at a time, each seed whose default branch moved since its
    /// snapshot. A commit whose build failed is not retried until the branch
    /// moves again or the daemon restarts; one whose soft steps alone failed
    /// built a current snapshot.
    pub async fn refresh_seeds(&self, daemon: &dyn Daemon) {
        let repositories = match self.list().await {
            Ok(repositories) => repositories,
            Err(error) => return log!("seed refresh: {error:#}"),
        };
        for repo in repositories {
            if let Err(error) = self.refresh_seed(daemon, &repo).await {
                let name = repo.display_name();
                let message = format!("Seed build for {name} failed: {error:#}");
                self.alert_seed_failed(daemon, About::named(None), message)
                    .await;
            }
        }
    }

    async fn refresh_seed(&self, daemon: &dyn Daemon, repo: &Repository) -> Result<()> {
        let layout = Layout::of(repo);
        let Some(plan) = self.seed_plan(repo, &layout).await? else {
            return self.discard_seed(repo, &layout).await;
        };
        let tip = git::seed::branch_commit(&repo.path, &plan.branch).await?;
        let build = (tip, plan.settings()?);
        let current = Snapshot::open(&layout)?
            .is_some_and(|snapshot| (snapshot.commit, snapshot.settings) == build);
        // Checked first: seeing any other build, a current one included,
        // forgets the failed one.
        let failed = self.seeds.repeats_failure(&repo.id, &build);
        if failed || current || !self.seed_headroom(daemon, &layout) {
            return Ok(());
        }
        let result = self.build_seed(repo, &layout, &plan, &build).await;
        self.seeds
            .record_result(&repo.id, result.is_err().then_some(build));
        self.report_soft_failures(daemon, repo, &result?).await;
        Ok(())
    }

    /// Tell the user which soft steps of the repository's installed seed
    /// failed, so new workspaces start without their outputs.
    async fn report_soft_failures(
        &self,
        daemon: &dyn Daemon,
        repo: &Repository,
        failures: &[StepFailure],
    ) {
        let name = repo.display_name();
        for failure in failures {
            let message = format!(
                "Seed build for {name} kept the outputs of its other steps: {failure}; \
                 new workspaces start without its outputs"
            );
            self.alert_seed_failed(daemon, About::named(None), message)
                .await;
        }
    }

    /// The repository's seed settings, or `None` when it configures no seed.
    pub(super) async fn seed_plan(
        &self,
        repo: &Repository,
        layout: &Layout,
    ) -> Result<Option<Plan>> {
        let settings = self.repository_settings(&repo.id).await?;
        Plan::new(repo, layout, &settings).await
    }

    /// Delete the seed of a repository that no longer configures one.
    async fn discard_seed(&self, repo: &Repository, layout: &Layout) -> Result<()> {
        if !layout.root().exists() {
            return Ok(());
        }
        let _seed = self.stop_seed_build(&repo.id).await;
        let _git = self.gates.lock_git(&repo.id).await;
        self.ensure_available(&repo.id).await?;
        self.remove_seed(repo).await?;
        self.seeds.record_result(&repo.id, None);
        Ok(())
    }

    /// Check the seed worktree out at `tip`, build it and freeze the outputs.
    /// Returns how its soft steps failed. Repository removal cancels the
    /// build and waits for it to return.
    async fn build_seed(
        &self,
        repo: &Repository,
        layout: &Layout,
        plan: &Plan,
        build: &(CommitId, String),
    ) -> Result<Vec<StepFailure>> {
        let tip = &build.0;
        let slot = self.seeds.slot(&repo.id);
        let _build = slot.gate().lock().await;
        if self.repository(&repo.id).await.is_err() {
            return Ok(Vec::new());
        }
        {
            let _git = self.gates.lock_git(&repo.id).await;
            self.ensure_available(&repo.id).await?;
            check_out_seed(repo, layout, tip).await?;
        }
        // Under the gate, which keeps cache emptying out until the build ends.
        shoal_core::caches::directories(repo, &plan.caches)?;
        let previous = previous_build(layout, tip).await?;
        log!("seed build for {} at {tip}", repo.path.display());
        let built = run_build(
            plan,
            previous.as_ref(),
            &layout.worktree(),
            &layout.log(),
            slot.cancelled(),
        )
        .await?;
        self.freeze_seed(repo, layout, plan, build, built.toolchains)
            .await?;
        // The new snapshot needs its own spare.
        self.seeds.want_spare();
        Ok(built.soft_failures)
    }

    /// Replace the snapshot with the outputs the seed worktree built with
    /// `toolchains`, cloning them while clones of the current snapshot go on
    /// and deleting the replaced one once nothing reads it. The caller holds the build slot,
    /// which keeps every other change to the snapshots out.
    async fn freeze_seed(
        &self,
        repo: &Repository,
        layout: &Layout,
        plan: &Plan,
        build: &(CommitId, String),
        toolchains: Toolchains,
    ) -> Result<()> {
        let staged = Snapshot::stage(layout, &plan.paths).await?;
        self.install_snapshot(repo, layout, staged, build, toolchains)
            .await?;
        let retired = layout.clone();
        off_runtime::run(move || retired.remove_retired()).await
    }

    /// Make `staged` the snapshot of the seed worktree built at `tip` with
    /// `toolchains`, checked under the Git gate while the repository is
    /// available, once the clones of the current snapshot end.
    async fn install_snapshot(
        &self,
        repo: &Repository,
        layout: &Layout,
        staged: Staged,
        (tip, settings): &(CommitId, String),
        toolchains: Toolchains,
    ) -> Result<()> {
        let slot = self.seeds.slot(&repo.id);
        let _snapshot = slot.snapshot().write().await;
        let _git = self.gates.lock_git(&repo.id).await;
        self.ensure_available(&repo.id).await?;
        let worktree = layout.worktree();
        ensure!(
            git::seed::head(&worktree).await? == *tip,
            "the seed worktree moved during its build"
        );
        let files = git::seed::unmodified_files(&worktree).await?;
        staged.install(
            layout,
            tip.to_owned().into(),
            settings.to_owned(),
            toolchains,
            files,
        )
    }

    /// Give each repository's current snapshot a spare clone of its outputs,
    /// which the next new workspace takes instead of cloning them.
    pub async fn refill_spares(&self, daemon: &dyn Daemon) {
        let repositories = match self.list().await {
            Ok(repositories) => repositories,
            Err(error) => return log!("seed spares: {error:#}"),
        };
        for repo in repositories {
            if let Err(error) = self.refill_spare(daemon, &repo.id).await {
                let name = repo.display_name();
                log!("seed spare for {name}: {error:#}");
            }
        }
    }

    /// Clone a spare of the repository's snapshot when it has none, its
    /// settings still describe the snapshot and the host has headroom for it.
    /// Starts clone and take the spare meanwhile.
    async fn refill_spare(&self, daemon: &dyn Daemon, id: &RepositoryId) -> Result<()> {
        let slot = self.seeds.slot(id);
        let _snapshot = slot.snapshot().read().await;
        // Read under the lock: a move or removal of the repository holds it.
        let Ok(repo) = self.repository(id).await else {
            return Ok(());
        };
        let layout = Layout::of(&repo);
        let Some(snapshot) = Snapshot::open(&layout)? else {
            return Ok(());
        };
        // A start meanwhile clones, as it would without spares.
        if layout.has_spare() || !self.seed_headroom(daemon, &layout) {
            return Ok(());
        }
        let Some(plan) = self.seed_plan(&repo, &layout).await? else {
            return Ok(());
        };
        if snapshot.settings == plan.settings()? {
            snapshot.refill_spare(&layout).await?;
        }
        Ok(())
    }

    /// Whether memory and disk space are clear of their overload warning
    /// thresholds, so a seed build or spare adds no load to a strained host.
    fn seed_headroom(&self, daemon: &dyn Daemon, layout: &Layout) -> bool {
        daemon.clear_of_overload(&[self.paths.state.as_path(), layout.root()])
    }

    /// Tell the user, or the coordinator of the space `about` names, that a
    /// seed failed.
    pub(super) async fn alert_seed_failed(
        &self,
        daemon: &dyn Daemon,
        about: About,
        message: String,
    ) {
        let alert = NewAlert::shoal(AlertKind::SeedFailed, about, message);
        self.notifications.notify(daemon, alert).await;
    }
}

/// What the current snapshot was built from, with the files changed between
/// its commit and `tip`, or `None` without a snapshot. The caller holds the
/// build slot, which keeps the snapshot from changing.
async fn previous_build(layout: &Layout, tip: &CommitId) -> Result<Option<Previous>> {
    let Some(snapshot) = Snapshot::open(layout)? else {
        return Ok(None);
    };
    let worktree = layout.worktree();
    let commit = snapshot.commit.as_str();
    let changed = if git::has_commit(&worktree, commit).await? {
        Some(git::seed::changed_between(&worktree, commit, tip.as_str()).await?)
    } else {
        None
    };
    Ok(Some(Previous {
        settings: snapshot.settings,
        toolchains: snapshot.toolchains,
        changed,
    }))
}

#[cfg(test)]
mod tests;
