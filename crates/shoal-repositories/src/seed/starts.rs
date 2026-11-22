//! Start a new workspace from its repository's seed snapshot, and give up
//! the outputs it took from there when it goes.
use std::path::Path;

use anyhow::Result;
use shoal_core::{
    config::Effective,
    fsutil::{
        off_runtime,
        subvolume::{self, Btrfs},
    },
    git,
};
use shoal_daemon_base::{log, notifications::About};
use shoal_protocol::model::{Repository, Workspace};

use super::{Layout, Snapshot, build, builds::Plan, state::SeedSlot};
use crate::{Repositories, daemon::Daemon};

impl Repositories {
    /// Start a new worktree from its repository's seed snapshot when the
    /// worktree's own settings describe the build that made it. The caller
    /// holds the workspace's creation gate and not the repository's Git gate.
    pub async fn apply_seed(
        &self,
        daemon: &dyn Daemon,
        repo: &Repository,
        workspace: &Workspace,
        settings: &Effective,
    ) {
        let layout = Layout::of(repo);
        let started = async {
            let Some(plan) = Plan::new(repo, &layout, settings).await? else {
                return Ok(());
            };
            let slot = self.seeds.slot(&repo.id);
            let _snapshot = slot.snapshot().read().await;
            let Some(snapshot) = Snapshot::open(&layout)? else {
                return Ok(());
            };
            layout.ensure_owned()?;
            if snapshot.settings != plan.settings()? {
                log!(
                    "{} starts without the seed: its settings differ",
                    workspace.name
                );
                return Ok(());
            }
            self.start_from_snapshot(&slot, &layout, &snapshot, &workspace.path, &plan.paths)
                .await
        };
        if let Err(error) = started.await {
            let about = About::space(&workspace.name, &workspace.id);
            let message = format!("Workspace starts without the seed build: {error:#}");
            self.alert_seed_failed(daemon, about, message).await;
        }
    }

    /// Give the new `worktree` the snapshot's outputs among `paths`: the
    /// spare's, which the spare task then replaces, or else a clone of them.
    /// The caller reads the snapshot under `slot`.
    async fn start_from_snapshot(
        &self,
        slot: &SeedSlot,
        layout: &Layout,
        snapshot: &Snapshot,
        worktree: &Path,
        paths: &[String],
    ) -> Result<()> {
        let spare = {
            let _taking = slot.spare().lock().await;
            snapshot.take_spare(layout, worktree, paths)?
        };
        let outputs = match spare {
            Some(taken) => {
                self.seeds.want_spare();
                taken
            }
            None => snapshot.clone_outputs(layout, worktree, paths).await?,
        };
        // Restored times are only safe beside the outputs built after them: on
        // outputs the worktree already had, they would hide changed sources.
        if outputs.is_empty() {
            return Ok(());
        }
        let changed =
            git::seed::changed_between(worktree, snapshot.commit.as_str(), "HEAD").await?;
        snapshot.restore_times(worktree, &changed)
    }

    /// Destroy the subvolumes among the seed outputs `workspace`'s settings
    /// name, which a worktree started from the seed holds, so removing the
    /// worktree need not delete their files one by one. An output holding a
    /// tracked file stays, so Git still finds a clean worktree clean, and
    /// settings or files it cannot read leave every output to that delete.
    pub async fn destroy_seed_outputs(&self, daemon: &dyn Daemon, workspace: &Workspace) {
        let Ok(settings) = self.workspace_settings(daemon, workspace).await else {
            return;
        };
        let presets = settings.setup.presets.as_deref().unwrap_or_default();
        let outputs = build::outputs(&settings.setup.seed, presets);
        let Ok(outputs) = git::seed::untracked_outputs(&workspace.path, &outputs).await else {
            return;
        };
        let root = workspace.path.clone();
        let destroyed = off_runtime::run(move || {
            subvolume::destroy_outputs(&Btrfs, &root, &outputs);
            Ok(())
        });
        let _ = destroyed.await;
    }
}
