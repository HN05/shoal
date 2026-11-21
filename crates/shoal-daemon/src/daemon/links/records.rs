//! What links read of a space in their own transactions: whether it takes
//! link changes, its place in the space tree and its checked branch.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use shoal_ids::space_ids::SpaceId;
use shoal_links::daemon::Records;
use shoal_protocol::model::SpaceRef;

use crate::daemon::tree::{self, TrackedAgents};

/// The space records links read, which the daemon keeps.
pub(crate) struct LinkRecords;

impl Records for LinkRecords {
    fn ensure_accepts_links(&self, db: &Connection, id: &SpaceId) -> Result<()> {
        super::ensure_accepts_links(db, id)
    }

    fn accepts_links(&self, db: &Connection, id: &SpaceId) -> Result<bool> {
        super::accepts_links(db, id)
    }

    fn forget_ready_marks(&self, db: &Connection, id: &SpaceId, urls: &[String]) -> Result<()> {
        crate::daemon::ready_marks::forget(db, id, urls)
    }

    fn require_ready_space(&self, db: &Connection, id: &SpaceId) -> Result<()> {
        super::space::require_ready_space(db, id)
    }

    fn delegated(
        &self,
        db: &Connection,
        id: &SpaceId,
        agents: &[SpaceId],
    ) -> Result<Vec<SpaceRef>> {
        tree::delegated(db, id, &TrackedAgents::of(agents))
    }

    fn receiving_paths(
        &self,
        db: &Connection,
        id: &SpaceId,
        agents: &[SpaceId],
    ) -> Result<Vec<Vec<SpaceId>>> {
        tree::receiving_paths(db, id, &TrackedAgents::of(agents))
    }

    fn delegated_elsewhere(
        &self,
        db: &Connection,
        agents: &[SpaceId],
    ) -> Result<Vec<(SpaceId, Vec<SpaceRef>)>> {
        tree::delegated_elsewhere(db, &TrackedAgents::of(agents))
    }

    fn subtree(&self, db: &Connection, id: &SpaceId) -> Result<Vec<SpaceId>> {
        tree::subtree(db, id)
    }

    fn checked_branch(&self, db: &Connection, id: &SpaceId) -> Result<Option<String>> {
        checked_branch(db, id)
    }
}

/// The branch of the workspace `id` while its conflicts are checked: a
/// chat, a workspace at a commit and one whose assignment is done have none.
fn checked_branch(db: &rusqlite::Connection, id: &SpaceId) -> Result<Option<String>> {
    Ok(db
        .query_row(
            "SELECT branch FROM workspaces WHERE id=?1
             AND NOT EXISTS (SELECT 1 FROM space_completion WHERE space_id=?1)",
            [id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten())
}
