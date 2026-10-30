//! Deleting the temporary directories of workspaces and chats once they are
//! gone.
use anyhow::Result;

use crate::{daemon::workspace::Manager, tmp_dirs::Layout};

impl Manager {
    /// Delete the temporary directory of the workspace or chat `id` of the
    /// repository `repository_id` once its record is gone.
    pub(crate) async fn delete_tmp_dir(&self, repository_id: &str, id: &str) -> Result<()> {
        let repo = self.repository(repository_id).await?;
        let Some(layout) = Layout::of(&repo) else {
            return Ok(());
        };
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || layout.remove_one(&id)).await?
    }
}

#[cfg(test)]
mod tests;
