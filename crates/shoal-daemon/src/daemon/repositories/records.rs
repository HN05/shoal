//! The records of other subsystems the repositories read and write in
//! their own transactions.
use std::{collections::BTreeMap, path::Path};

use anyhow::Result;
use rusqlite::{Connection, Transaction, params};
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_repositories::daemon::{Activity, Records};

use crate::{
    daemon::{
        leases::DaemonRecords,
        resources::{self, Scope},
        store,
    },
    model::Workspace,
    state::{QueueKind, QueueState, WorkspaceState},
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

    fn repository_activity(&self, db: &Connection, id: &RepositoryId) -> Result<Activity> {
        let checkout_leased = store::exists(
            db,
            "SELECT 1 FROM repository_resource_leases WHERE repository_id=?1",
            [id],
        )?;
        let chats = store::exists(
            db,
            "SELECT 1 FROM spaces WHERE repository_id=?1 AND kind='chat'",
            [id],
        )?;
        // A job waiting for the runner holds nothing in the directory, and
        // claiming it creates its workspace under the Git gate a move holds.
        let launches = store::exists(
            db,
            "SELECT 1 FROM agent_queue WHERE repository_id=?1 AND NOT (kind=?2 AND state=?3)",
            params![id, QueueKind::Job, QueueState::Queued],
        )?;
        let workspace_states = db
            .prepare(
                "SELECT id,state FROM spaces JOIN workspaces USING(id) WHERE repository_id=?1",
            )?
            .query_map([id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<BTreeMap<SpaceId, WorkspaceState>>>()?;
        Ok(Activity {
            checkout_leased,
            chats,
            launches,
            workspace_states,
        })
    }

    fn reconcile_workspaces(&self, tx: &Transaction<'_>, id: &RepositoryId) -> Result<()> {
        tx.execute(
            "UPDATE workspaces SET state=?2 WHERE id IN (SELECT id FROM spaces WHERE repository_id=?1)",
            params![id, WorkspaceState::Reconciling],
        )?;
        Ok(())
    }

    fn restore_workspace_state(
        &self,
        tx: &Transaction<'_>,
        id: &SpaceId,
        state: WorkspaceState,
    ) -> Result<()> {
        // A daemon restart quarantined it with an error that no longer
        // applies once the move finished.
        tx.execute(
            "UPDATE workspaces SET state=?2,
                error=CASE WHEN ?2=?3 THEN NULL ELSE error END
                WHERE id=?1",
            params![id, state, WorkspaceState::Ready],
        )?;
        Ok(())
    }

    fn rebase_paths(
        &self,
        tx: &Transaction<'_>,
        id: &RepositoryId,
        source: &Path,
        destination: &Path,
    ) -> Result<()> {
        let (from, to) = (source.to_string_lossy(), destination.to_string_lossy());
        tx.execute(
            &format!(
                "UPDATE workspaces SET path={}, git_dir={}
                    WHERE id IN (SELECT id FROM spaces WHERE repository_id=?1)",
                store::path_rebased("path"),
                store::path_rebased("git_dir")
            ),
            params![id, from, to],
        )?;
        crate::daemon::alerts::rebase_attachments(tx, source, destination)?;
        crate::daemon::workspace_files::rebase(tx, id, source, destination)?;
        crate::daemon::approvals::proposals::rebase_files(tx, source, destination)
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
