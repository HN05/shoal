//! What reviews need of the rest of the daemon, which implements it.
use anyhow::Result;
use rusqlite::Connection;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceName;

/// The records of the spaces reviews are of, which reviews read in the
/// connection or transaction that reads the reviews, so a review and its
/// workspace are read as one. Its methods run on the store's thread, so
/// they are not futures.
pub trait Records: Send + Sync {
    /// The name of the space `id`, while it exists.
    fn space_name(&self, db: &Connection, id: &SpaceId) -> Result<Option<SpaceName>>;

    /// The workspaces of `repository`, or every workspace without one.
    fn workspaces(
        &self,
        db: &Connection,
        repository: Option<&RepositoryId>,
    ) -> Result<Vec<SpaceId>>;
}
