//! The steps of a move that the daemon's tests interrupt between.
use std::path::Path;

use anyhow::Result;
use shoal_ids::repository_ids::RepositoryId;

use crate::Repositories;

impl Repositories {
    /// Record the move of the repository `id` from `source` to
    /// `destination`, as a move does before its directory moves.
    pub async fn record_move_intent(
        &self,
        id: &RepositoryId,
        source: &Path,
        destination: &Path,
    ) -> Result<()> {
        self.record_move(id, source, destination).await
    }

    /// Point the records at `destination`, as a move does once its
    /// directory got there.
    pub async fn switch_move_records(
        &self,
        id: &RepositoryId,
        source: &Path,
        destination: &Path,
    ) -> Result<()> {
        self.switch_move(id, source, destination).await
    }
}
