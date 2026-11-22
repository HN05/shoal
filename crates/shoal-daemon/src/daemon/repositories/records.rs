//! The records of other subsystems the repositories read and write in
//! their own transactions.
use anyhow::Result;
use rusqlite::{Connection, Transaction};
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_repositories::daemon::Records;

use crate::{
    daemon::{
        leases::DaemonRecords,
        resources::{self, Scope},
        store,
    },
    model::Workspace,
};

pub(crate) struct RepositoryRecords;

impl Records for RepositoryRecords {
    fn workspaces(&self, db: &Connection) -> Result<Vec<Workspace>> {
        super::super::workspace::workspaces_within(db, None)
    }

    fn removal_depth(&self, db: &Connection, id: &SpaceId) -> Result<u32> {
        super::super::tree::removal_depth(db, id)
    }

    fn ensure_no_external_leases(&self, db: &Connection, id: &RepositoryId) -> Result<()> {
        resources::ensure_no_external_leases(db, &DaemonRecords, id)
    }

    fn repository_in_use(&self, db: &Connection, id: &RepositoryId) -> Result<bool> {
        Ok(recorded_use(db, id)? || resources::borrowed(db, &DaemonRecords, id)?)
    }

    fn forget_repository_pools(&self, tx: &Transaction<'_>, id: &RepositoryId) -> Result<()> {
        tx.execute(
            "DELETE FROM resource_pools WHERE scope=?1",
            [Scope::Repo(id.clone())],
        )?;
        Ok(())
    }
}

/// Whether the repository `id` still has a space, a queued agent, a pending
/// issue proposal, a kept branch, a job waiting to rerun, or an unexpired
/// checkpoint, removal record or removed space's files.
fn recorded_use(db: &Connection, id: &RepositoryId) -> Result<bool> {
    store::exists(
        db,
        "SELECT 1 FROM spaces WHERE repository_id=?1
        UNION ALL SELECT 1 FROM agent_queue WHERE repository_id=?1
        UNION ALL SELECT 1 FROM access_requests WHERE repository_id=?1 AND active=1
        UNION ALL SELECT 1 FROM kept_branches WHERE repository_id=?1
        UNION ALL SELECT 1 FROM ci_job_reruns WHERE repository_id=?1
        UNION ALL SELECT 1 FROM checkpoints
            WHERE repository_id=?1 AND expires_at > unixepoch()
        UNION ALL SELECT 1 FROM removed_spaces
            WHERE repository_id=?1 AND expires_at > unixepoch()
        UNION ALL SELECT 1 FROM removed_space_files JOIN space_files USING(space_id)
            WHERE repository_id=?1 AND expires_at > unixepoch()",
        [id],
    )
}
