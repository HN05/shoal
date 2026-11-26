//! The directories spaces' agents work in, as this machine finds them.
use crate::Spaces;
use std::{collections::HashSet, path::Path};

use anyhow::{Result, ensure};
use futures_util::StreamExt;
use shoal_ids::space_ids::SpaceId;

use shoal_protocol::space_names::SpaceSelector;

use crate::model::Space;

/// Each runs on the blocking pool.
const DIRECTORY_CHECKS: usize = 8;

impl Spaces {
    /// Refuses a space whose agent's directory is not found.
    pub async fn entered_space(&self, selector: &SpaceSelector) -> Result<Space> {
        let space = self.space(selector).await?;
        let directory = space.workdir.path();
        ensure!(
            self.has_directory(directory).await?,
            "the directory of space {space} is missing: {}",
            directory.display()
        );
        Ok(space)
    }

    /// Checks a few at a time. One whose file access stalls counts as
    /// missing.
    pub async fn directory_spaces(&self, spaces: &[Space]) -> HashSet<SpaceId> {
        let checks: Vec<_> = spaces.iter().map(|space| self.found_space(space)).collect();
        let found = futures_util::stream::iter(checks).buffer_unordered(DIRECTORY_CHECKS);
        found.filter_map(std::future::ready).collect().await
    }

    async fn found_space(&self, space: &Space) -> Option<SpaceId> {
        let found = self.has_directory(space.workdir.path()).await;
        found.unwrap_or(false).then(|| space.id.clone())
    }

    /// Checked under the limit file access in a worktree runs under.
    async fn has_directory(&self, path: &Path) -> Result<bool> {
        let owned = path.to_owned();
        let check = async move { Ok(tokio::task::spawn_blocking(move || owned.is_dir()).await?) };
        self.file_access
            .checked(&self.notifications, path, check)
            .await
    }
}
