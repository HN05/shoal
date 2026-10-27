//! The named checks and views that confine a scoped caller to what its
//! scope allows.
use anyhow::{Result, ensure};

use super::Caller;
use crate::{
    daemon::workspace::Manager,
    model::{Repository, Workspace, WorkspaceTarget},
    repository::Selector,
};

impl Manager {
    /// The repositories `caller` may see: its own workspace's, or all of them
    /// for an unscoped caller.
    pub(in crate::daemon) async fn visible_repositories(
        &self,
        caller: Option<&Caller>,
    ) -> Result<Vec<Repository>> {
        let mut repositories = self.repositories().await?;
        if let Some(caller) = caller {
            let own = self.workspace(&caller.workspace_id).await?.repository_id;
            repositories.retain(|repository| repository.id == own);
        }
        Ok(repositories)
    }

    /// The workspaces `target` finds among those `caller` may see: the ones
    /// in its workspace tree, or all of them for an unscoped caller.
    pub(in crate::daemon) async fn find_visible_workspaces(
        &self,
        target: &WorkspaceTarget,
        caller: Option<&Caller>,
    ) -> Result<Vec<Workspace>> {
        let within = match caller {
            Some(caller) => Some(self.subtree(&caller.workspace_id).await?),
            None => None,
        };
        self.find_workspaces(target, within).await
    }

    /// `workspace`, which must be the caller's own; `refusal` says why
    /// another is refused.
    pub(in crate::daemon) async fn own_workspace<'a>(
        &self,
        caller: &Caller,
        workspace: &'a mut String,
        refusal: &str,
    ) -> Result<&'a mut String> {
        ensure!(
            self.workspace(workspace).await?.id == caller.workspace_id,
            "{refusal}"
        );
        Ok(workspace)
    }

    /// `workspace`, which must not be the caller's own; [`super::authorize`] confines
    /// it to the workspaces below. `refusal` says why the caller's own is
    /// refused.
    pub(in crate::daemon) async fn workspace_below<'a>(
        &self,
        caller: &Caller,
        workspace: &'a mut String,
        refusal: &str,
    ) -> Result<&'a mut String> {
        ensure!(
            self.workspace(workspace).await?.id != caller.workspace_id,
            "{refusal}"
        );
        Ok(workspace)
    }

    /// The caller's own repository, which `repository` must select.
    pub(in crate::daemon) async fn own_repository(
        &self,
        caller: &Caller,
        repository: &Selector,
    ) -> Result<Selector> {
        let own = self.workspace(&caller.workspace_id).await?.repository_id;
        ensure!(
            self.repository(repository).await?.id == own,
            "workspace processes can only sync their own repository"
        );
        Ok(own.into())
    }
}
