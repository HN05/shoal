//! Bringing a workspace that holds nothing of its own up to date with what
//! it was made from. See `docs/design/spaces.md`.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::{Context, Result, ensure};

use super::super::workspace::copies::Source;
use crate::{
    git,
    model::{Refreshed, Repository, Workspace},
    state::WorkspaceState,
    {log, scope::Caller},
};
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::space_names::SpaceSelector;

mod own_work;
mod target;

use own_work::{HoldsOwnWork, OwnWork};
use target::{Found, Move, Target};

impl Spaces {
    /// Refuses a workspace that holds work of its own, or where an execution
    /// other than `caller`'s own runs.
    pub async fn refresh_workspace(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        caller: Option<&Caller>,
    ) -> Result<Refreshed> {
        let workspace = self.workspace(selector).await?;
        let target = self.refresh_target(daemon, &workspace, caller).await?;
        if matches!(target, Target::DefaultBranch) {
            self.sync_default_branch(daemon, &workspace).await;
        }
        self.refresh_to(daemon, &workspace, target, caller).await
    }

    /// Runs before an admitted agent starts, as `shoal refresh` does, with
    /// what the checkout has of the default branch. A fast-forward that
    /// cannot happen leaves the branch as it was. A workspace at a commit, a
    /// copy, whose preparation already brought it up to date, or one that
    /// holds work of its own stays as it is. One that cannot be refreshed
    /// starts its agent as it is.
    pub async fn refresh_admitted(&self, daemon: &dyn Daemon, workspace: &Workspace) {
        if workspace.copied_from.is_some() || workspace.branch().is_none() {
            return;
        }
        let refreshed = async {
            let target = self.refresh_target(daemon, workspace, None).await?;
            self.refresh_to(daemon, workspace, target, None).await
        };
        if let Err(error) = refreshed.await
            && !error.is::<HoldsOwnWork>()
        {
            let name = &workspace.name;
            log!("{name} starts its agent without a refresh: {error:#}");
        }
    }

    async fn refresh_to(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        target: Target,
        caller: Option<&Caller>,
    ) -> Result<Refreshed> {
        let (repo, _git) = daemon
            .lock_repository(crate::repository::Selector::from(workspace.repository()))
            .await?;
        let reserved = self
            .reserve_lifecycle(&workspace.id, WorkspaceState::Updating)
            .await?;
        // A rename may have finished while this request waited.
        let workspace = self.workspace_by_id(&workspace.id).await?;
        let own = caller.and_then(Caller::execution_id);
        let result = self
            .refresh_reserved(daemon, &repo, &workspace, reserved, target, own)
            .await;
        self.set_state(&workspace.id, reserved, workspace.error.clone())
            .await?;
        let refreshed = result?;
        self.touch(&workspace.id).await;
        Ok(Refreshed {
            workspace: self.workspace_by_id(&workspace.id).await?,
            ..refreshed
        })
    }

    /// The sync takes the repository's Git gate itself and records a failure
    /// for status to show. The refresh then goes on with the local default
    /// branch as the checkout has it.
    async fn sync_default_branch(&self, daemon: &dyn Daemon, workspace: &Workspace) {
        let repository = (workspace.repository()).into();
        if let Err(error) = daemon.sync_repository(&repository).await {
            log!(
                "refresh of {} goes on with the local default branch: {error:#}",
                workspace.name
            );
        }
    }

    /// Waits until nothing but the execution `own` runs in the workspace.
    async fn refresh_reserved(
        &self,
        daemon: &dyn Daemon,
        repo: &Repository,
        workspace: &Workspace,
        state: WorkspaceState,
        target: Target,
        own: Option<&ExecutionId>,
    ) -> Result<Refreshed> {
        self.ensure_refreshable(daemon, workspace, state, own)
            .await?;
        let path = &workspace.path;
        let from = git::resolve_commit(path, "HEAD", git::run_isolated).await?;
        let (target, up_to_date, setup) = match self.find_target(daemon, repo, target).await? {
            Found::Fixed(what) => (what, true, false),
            Found::Moving(moving) => {
                let moved = self.move_to_target(daemon, workspace, &moving).await?;
                (moving.label, !moved, false)
            }
            Found::Copy { source, fresh } => {
                let copied = self
                    .copy_source_again(daemon, workspace, &source, &fresh)
                    .await?;
                // The source's ignored files replaced what its setup wrote.
                let setup = copied && !workspace.setup_skipped;
                (format!("workspace {}", source.name()), !copied, setup)
            }
        };
        Ok(Refreshed {
            workspace: workspace.clone(),
            target,
            to: git::resolve_commit(path, "HEAD", git::run_isolated).await?,
            from,
            up_to_date,
            setup,
        })
    }

    /// The workspace must be ready, its worktree must verify with no Git
    /// operation under way, and no execution but `own` may run in it.
    async fn ensure_refreshable(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        state: WorkspaceState,
        own: Option<&ExecutionId>,
    ) -> Result<()> {
        crate::target_checks::ready(workspace, state).require()?;
        self.ensure_no_other_execution(workspace, own).await?;
        let git_dir = self.verified_git_dir(daemon, workspace).await?;
        ensure!(
            !git::status::operation_in_progress_at(&workspace.path, &git_dir).await?,
            "a Git operation is under way in workspace {}; finish or abort it first",
            workspace.name
        );
        Ok(())
    }

    /// Does nothing when the copy has what it started with. Returns whether
    /// it copied. Work of the copy's own refuses it, naming `fresh`, which
    /// makes a fresh copy.
    async fn copy_source_again(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        source: &Source,
        fresh: &str,
    ) -> Result<bool> {
        let copied = (workspace.copied_from.as_ref()).context("workspace is no copy")?;
        if self.source_unchanged(source, copied).await? {
            return Ok(false);
        }
        OwnWork::copy(copied.head.as_str(), fresh)
            .refuse(workspace)
            .await?;
        self.copy_again(daemon, workspace, source).await?;
        Ok(true)
    }

    /// Returns whether it moved. Work of its own refuses the move.
    async fn move_to_target(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        moving: &Move,
    ) -> Result<bool> {
        let (path, commit) = (&workspace.path, moving.commit.as_str());
        match &moving.how {
            target::How::FastForward => {
                if git::is_ancestor(path, commit, "HEAD", git::isolated_command).await? {
                    return Ok(false);
                }
                OwnWork::branch(commit, &moving.label)
                    .refuse(workspace)
                    .await?;
                self.fast_forward_to(daemon, workspace, commit).await?;
            }
            target::How::Restart => {
                let at = workspace.at().context("workspace records no start")?;
                if at.commit == moving.commit {
                    return Ok(false);
                }
                OwnWork::at_commit(at.commit.as_str())
                    .refuse(workspace)
                    .await?;
                self.move_to_start(workspace, moving.commit.clone()).await?;
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests;
