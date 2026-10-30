//! Deleting the temporary directories of workspaces once they are gone.
use anyhow::Result;

use crate::{daemon::workspace::Manager, model::Workspace, tmp_dirs::Layout};

impl Manager {
    /// Delete `workspace`'s temporary directory once it is removed.
    pub(crate) async fn delete_workspace_tmp_dir(&self, workspace: &Workspace) -> Result<()> {
        let repo = self.repository(&workspace.repository_id).await?;
        let Some(layout) = Layout::of(&repo) else {
            return Ok(());
        };
        let id = workspace.id.clone();
        tokio::task::spawn_blocking(move || layout.remove_one(&id)).await?
    }
}

#[cfg(test)]
mod tests;
