//! The startup audit of workspace ownership, and the cleanup sweep's
//! recheck of the workspaces it failed, which a worktree on a volume that
//! mounted late recovers through.
use anyhow::Result;

use crate::{
    daemon::{
        log,
        workspace::{Manager, VerifiedWorktree},
    },
    model::Workspace,
    state::WorkspaceState,
};

/// How the startup audit's workspace errors begin, which the recheck looks for.
const STARTUP_OWNERSHIP_FAILURE: &str = "startup ownership check failed";

impl Manager {
    /// Startup audit never deletes work, releases leases, or assumes unknown
    /// executions are dead. Invalid ready workspaces are quarantined as failed.
    pub async fn audit_worktrees(&self) -> Result<()> {
        for workspace in self.list_workspaces().await? {
            if workspace.state != WorkspaceState::Ready {
                continue;
            }
            match self.verify_and_record_worktree(&workspace, true).await {
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
    pub async fn recheck_startup_failures(&self) -> Result<()> {
        for workspace in self.list_workspaces().await? {
            if !failed_at_startup(&workspace) {
                continue;
            }
            if let Err(error) = self.recheck_startup_failure(&workspace).await {
                log!("ownership recheck skipped {}: {error:#}", workspace.name);
            }
        }
        Ok(())
    }

    async fn recheck_startup_failure(&self, workspace: &Workspace) -> Result<()> {
        let Some(_git) = self.gates.try_lock_git(&workspace.repository_id) else {
            return Ok(());
        };
        // Check without reserving first, so a workspace that still fails
        // never leaves the failed state for another operation to trip over.
        if self.verify_worktree(workspace).await.is_err() {
            return Ok(());
        }
        let previous = self
            .reserve_lifecycle(&workspace.id, WorkspaceState::Reconciling)
            .await?;
        // Another operation may have changed the workspace since it was listed.
        let current = self.workspace(&workspace.id).await?;
        let verified = failed_at_startup(&Workspace {
            state: previous,
            ..current.clone()
        }) && self
            .verify_and_record_worktree(&current, true)
            .await
            .is_ok();
        if verified {
            self.set_state(&workspace.id, WorkspaceState::Ready, None)
                .await?;
            log!("ownership of {} verified again; ready", workspace.name);
        } else {
            self.set_state(&workspace.id, previous, current.error)
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
