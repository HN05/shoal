//! The startup audit of workspace ownership, and the cleanup sweep's
//! recheck of the workspaces it failed, which a worktree on a volume that
//! mounted late, or whose file access stalled, recovers through.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::Result;

use crate::{
    model::Workspace,
    state::WorkspaceState,
    {log, workspace::VerifiedWorktree},
};

/// How the startup audit's workspace errors begin, which the recheck looks for.
pub const STARTUP_OWNERSHIP_FAILURE: &str = "startup ownership check failed";

impl Spaces {
    /// Startup audit never deletes work, releases leases, or assumes unknown
    /// executions are dead. Invalid ready workspaces are quarantined as failed.
    pub async fn audit_worktrees(&self, daemon: &dyn Daemon) -> Result<()> {
        for workspace in self.list_workspaces().await? {
            if workspace.state != WorkspaceState::Ready {
                continue;
            }
            let verified = self.verify_and_record_worktree(daemon, &workspace, true);
            match self
                .file_access
                .checked(&self.notifications, &workspace.path, verified)
                .await
            {
                Err(error) => {
                    self.set_state(
                        &workspace.id,
                        WorkspaceState::Failed,
                        Some(format!("{STARTUP_OWNERSHIP_FAILURE}: {error:#}")),
                    )
                    .await?;
                }
                Ok(VerifiedWorktree {
                    identity: Err(error),
                }) => log!(
                    "worktree identity not recorded for {}: {error:#}",
                    workspace.name
                ),
                Ok(_) => {}
            }
        }
        Ok(())
    }

    /// Make each workspace the startup audit failed ready again once its
    /// worktree verifies. Other failures, such as a failed setup, stay for
    /// the user, and a repository whose Git gate is busy waits for the next pass.
    pub async fn recheck_startup_failures(&self, daemon: &dyn Daemon) -> Result<()> {
        for workspace in self.list_workspaces().await? {
            if !failed_at_startup(&workspace) {
                continue;
            }
            if let Err(error) = self.recheck_startup_failure(daemon, &workspace).await {
                log!("ownership recheck skipped {}: {error:#}", workspace.name);
            }
        }
        Ok(())
    }

    async fn recheck_startup_failure(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
    ) -> Result<()> {
        let Some(_git) = self.gates.try_lock_git(workspace.repository()) else {
            return Ok(());
        };
        // Check without reserving first, so a workspace that still fails
        // never leaves the failed state for another operation to trip over.
        let verified = self.verify_worktree(daemon, workspace);
        if self
            .file_access
            .checked(&self.notifications, &workspace.path, verified)
            .await
            .is_err()
        {
            return Ok(());
        }
        let previous = self
            .reserve_lifecycle(&workspace.id, WorkspaceState::Reconciling)
            .await?;
        // Another operation may have changed the workspace since it was listed.
        let current = self.workspace_by_id(&workspace.id).await?;
        let mut listed = current.clone();
        listed.state = previous;
        let verified = failed_at_startup(&listed)
            && self
                .file_access
                .checked(
                    &self.notifications,
                    &current.path,
                    self.verify_and_record_worktree(daemon, &current, true),
                )
                .await
                .is_ok();
        if verified {
            self.set_state(&workspace.id, WorkspaceState::Ready, None)
                .await?;
            log!("ownership of {} verified again; ready", workspace.name);
        } else {
            self.set_state(&workspace.id, previous, current.part.error)
                .await?;
        }
        Ok(())
    }
}

/// Whether the startup audit failed the workspace and nothing changed it since.
fn failed_at_startup(workspace: &Workspace) -> bool {
    workspace.state == WorkspaceState::Failed
        && workspace
            .error
            .as_deref()
            .is_some_and(|error| error.starts_with(STARTUP_OWNERSHIP_FAILURE))
}

#[cfg(test)]
mod tests;
