//! The spaces whose settings the repositories read, that a repository's
//! removal takes, and that a move of its directory or of `root_dir`
//! carries along.
use std::path::{Path, PathBuf};

use anyhow::{Result, ensure};
use futures_util::future::BoxFuture;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;
use shoal_repositories::{
    daemon::Spaces,
    moves::{Running, chats::OutsideChat},
};

use crate::{
    daemon::{log, manager::Manager},
    fsutil::off_runtime,
    model::{RepositoryRemoval, Space, Workspace},
    state::WorkspaceState,
};

impl Spaces for Manager {
    fn space<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Space>> {
        Box::pin(Manager::space(self, selector))
    }

    fn find_space<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<Space>>> {
        Box::pin(Manager::find_space(self, id))
    }

    fn ensure_removable_workspace<'a>(
        &'a self,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(ensure_removable(self, workspace))
    }

    fn ensure_movable_workspace<'a>(
        &'a self,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(ensure_movable(self, workspace))
    }

    fn record_moved_worktree<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let verified = self.verify_and_record_worktree(workspace, true).await?;
            if let Err(error) = verified.identity {
                log!(
                    "worktree identity not recorded for {}: {error:#}",
                    workspace.name
                );
            }
            Ok(())
        })
    }

    fn outside_dirs(&self, root: &Path) -> Vec<PathBuf> {
        Manager::outside_dirs(self, root)
    }

    fn outside_chats_moving<'a>(
        &'a self,
        root_dir: &'a Path,
        destination: &'a Path,
        running: Running,
    ) -> BoxFuture<'a, Result<Vec<OutsideChat>>> {
        Box::pin(Manager::outside_chats_moving(
            self,
            root_dir,
            destination,
            running,
        ))
    }

    fn move_outside_chats<'a>(
        &'a self,
        chats: Vec<OutsideChat>,
        root_dir: &'a Path,
        destination: &'a Path,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(Manager::move_outside_chats(
            self,
            chats,
            root_dir,
            destination,
        ))
    }

    fn remove_for_repository<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            Manager::remove_for_repository(self, id).await?;
            Ok(())
        })
    }

    fn hold_handovers<'a>(
        &'a self,
        work: BoxFuture<'a, Result<Option<RepositoryRemoval>>>,
    ) -> BoxFuture<'a, Result<Option<RepositoryRemoval>>> {
        Box::pin(self.conversion_handovers.hold(work))
    }

    fn local_config_changed(&self) {
        // The runner follows `runner.enabled` as soon as it changes.
        self.runner.jobs().settings_changed();
    }
}

/// Refuse a workspace the repository's removal cannot take: one an
/// operation holds, other than a preparation whose setup has yet to start,
/// which the caller's Git gate keeps from starting, or one whose worktree
/// is not verifiably its own. A worktree its preparation has yet to create
/// holds nothing to verify.
async fn ensure_removable(manager: &Manager, workspace: &Workspace) -> Result<()> {
    let idle = match workspace.state {
        WorkspaceState::Ready | WorkspaceState::Failed => true,
        WorkspaceState::Preparing => !manager.workspace_has_executions(&workspace.id).await?,
        _ => false,
    };
    ensure!(idle, "workspace {} is busy", workspace.name);
    if off_runtime::exists(&workspace.path).await? {
        let common_dir = manager.common_dir(workspace).await?;
        let workspace = manager.recover_git_dir(workspace, &common_dir).await?;
        manager.verify_worktree(&workspace).await?;
    } else if !workspace.checkout.unborn() {
        ensure!(
            workspace.state == WorkspaceState::Failed,
            "workspace {} is missing; reconcile it before repository removal",
            workspace.name
        );
        ensure!(
            manager.missing_worktree(workspace).await?.is_none(),
            "workspace {} was moved; restore it before repository removal",
            workspace.name
        );
    }
    Ok(())
}

/// Refuse a workspace its repository's move cannot take: one whose
/// worktree is not verifiably its own, or was moved.
async fn ensure_movable(manager: &Manager, workspace: &Workspace) -> Result<()> {
    if off_runtime::exists(&workspace.path).await? {
        return manager.verify_worktree(workspace).await;
    }
    ensure!(
        manager.missing_worktree(workspace).await?.is_none(),
        "workspace {} was moved; restore it before moving its repository",
        workspace.name
    );
    Ok(())
}
