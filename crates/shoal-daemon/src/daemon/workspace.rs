//! The workspace records the daemon reads.
mod adoption;
mod branch_rename;
mod branches;
pub(super) mod checkpoints;
pub(super) mod commits;
pub(crate) mod conflicts;
mod converted;
mod copies;
pub(super) mod creation;
mod diff;
mod disowned;
mod fast_forward;
pub(in crate::daemon) mod identity;
mod kept_branches;
mod location;
mod merge;
pub(super) mod ownership;
pub(crate) mod paths;
mod refresh;
pub(super) mod reservation;
mod stack;
mod status;
pub(super) mod usage;
mod xcode;

pub(crate) use branches::NewWorktreeOptions;
pub(crate) use copies::CopyPlace;
pub(crate) use creation::{NewWorkspace, build_pending_in};
pub(crate) use merge::RecordChange;
pub(crate) use ownership::VerifiedWorktree;
pub(crate) use stack::record_base_changed;

use super::manager::Manager;
use crate::{
    daemon::{spaces::status::Records, store},
    model::{Execution, Inspection, Workspace},
};
use anyhow::Result;
use rusqlite::OptionalExtension;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;
use std::collections::HashSet;

impl Manager {
    pub async fn list_workspaces(&self) -> Result<Vec<Workspace>> {
        self.list_workspaces_within(None).await
    }

    /// `root` and every workspace below it, or every workspace without
    /// `root`, by name. Only the selected workspaces load their details.
    pub async fn list_workspaces_within(&self, root: Option<&SpaceId>) -> Result<Vec<Workspace>> {
        let root = root.cloned();
        self.store
            .run(move |db| workspaces_within(db, root.as_ref()))
            .await
    }

    pub async fn workspace_ids(&self) -> Result<HashSet<SpaceId>> {
        self.store
            .run(|db| {
                Ok(db
                    .prepare("SELECT id FROM spaces WHERE kind='workspace'")?
                    .query_map([], |row| row.get(0))?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
    }

    /// What the workspace's processes use, from the latest sample.
    pub(crate) fn workspace_usage(&self, id: &SpaceId) -> Option<crate::model::WorkspaceUsage> {
        let now = tokio::time::Instant::now();
        self.usage.with_workspaces(|usages| usages.get(id, now))
    }

    /// Whether the workspace has any recorded execution, in any state.
    pub(super) async fn workspace_has_executions(&self, id: &SpaceId) -> Result<bool> {
        let id = id.to_owned();
        self.store
            .run(move |db| store::exists(db, "SELECT 1 FROM executions WHERE space_id=?1", [id]))
            .await
    }

    /// Read recorded executions in every state for a known workspace ID.
    pub async fn workspace_executions(&self, id: &SpaceId) -> Result<Vec<Execution>> {
        let id = id.to_owned();
        self.store.run(move |db| store::executions(db, &id)).await
    }

    /// Count recorded executions in every state without decoding their details.
    pub(super) async fn workspace_execution_count(&self, id: &SpaceId) -> Result<usize> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                let count: i64 = db.query_row(
                    "SELECT COUNT(*) FROM executions WHERE space_id=?1",
                    [id],
                    |row| row.get(0),
                )?;
                Ok(usize::try_from(count)?)
            })
            .await
    }

    /// The workspace with this ID, unless it was removed, read in one store
    /// operation, so a removal meanwhile reads as none.
    pub async fn find_workspace(&self, id: &SpaceId) -> Result<Option<Workspace>> {
        let id = id.to_owned();
        self.store.run(move |db| recorded_workspace(db, &id)).await
    }

    /// Resolve an optional selector to a workspace ID filter.
    pub async fn workspace_filter(
        &self,
        selector: Option<&SpaceSelector>,
    ) -> Result<Option<SpaceId>> {
        match selector {
            Some(selector) => Ok(Some(self.workspace(selector).await?.id)),
            None => Ok(None),
        }
    }

    pub async fn inspect_workspace(&self, selector: &SpaceSelector) -> Result<Inspection> {
        let workspace = self.workspace(selector).await?;
        let simulators = self.list_simulators(Some(&workspace.id)).await?;
        let Records {
            issues,
            pr_cleanup,
            completion,
        } = self.space_records(&workspace.id).await?;
        let mut inspection = self
            .store
            .run(move |db| {
                Ok(Inspection {
                    issues,
                    completion,
                    pr_cleanup,
                    executions: store::executions(db, &workspace.id)?,
                    places: super::places::owned(db, Some(&workspace.id))?
                        .into_iter()
                        .map(|(_, place)| place)
                        .collect(),
                    ports: store::ports(db, Some(&workspace.id))?,
                    resources: crate::daemon::resources::leases(db, Some(&workspace.id))?,
                    simulators,
                    workspace,
                })
            })
            .await?;
        self.annotate_stops(&mut inspection.executions).await;
        Ok(inspection)
    }

    /// Record activity so automatic cleanup restarts its idle timer, and its
    /// time for idle periods that must survive a restart.
    pub async fn touch(&self, id: &SpaceId) {
        self.usage.record_activity(id);
        let id = id.to_owned();
        let recorded = self
            .store
            .run(move |db| {
                db.execute(
                    "UPDATE workspaces SET active_at=unixepoch() WHERE id=?1",
                    [id],
                )?;
                Ok(())
            })
            .await;
        if let Err(error) = recorded {
            crate::daemon::log!("activity time not recorded: {error:#}");
        }
    }

    /// When the space `id` was last recorded active: a workspace without
    /// any starts its idle period now, while a chat records no such time.
    pub(crate) async fn active_at(&self, id: &SpaceId) -> Result<Option<crate::time::UnixTime>> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE workspaces SET active_at=unixepoch() WHERE id=?1 AND active_at IS NULL",
                    [&id],
                )?;
                Ok(db
                    .query_row(
                        "SELECT active_at FROM workspaces WHERE id=?1",
                        [&id],
                        |row| row.get(0),
                    )
                    .optional()?)
            })
            .await
    }

    /// Drop in-memory bookkeeping for a workspace that no longer exists.
    pub(crate) async fn forget_workspace(&self, id: &SpaceId) {
        self.usage.forget_workspace(id);
        self.rejected_configs.forget(id);
        self.executions.forget_workspace(id).await;
    }
}

/// Fill in what a workspace record reads from its related tables.
/// `root` and every workspace below it, or every workspace without `root`,
/// by name, each with its details.
pub(crate) fn workspaces_within(
    db: &rusqlite::Connection,
    root: Option<&SpaceId>,
) -> Result<Vec<Workspace>> {
    let mut workspaces = db
        .prepare(&super::tree::subtree_query(store::WORKSPACE_COLUMNS))?
        .query_map([root], store::workspace)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for workspace in &mut workspaces {
        load_details(db, workspace)?;
    }
    Ok(workspaces)
}

/// The workspace `id` with its details, while it exists.
fn recorded_workspace(db: &rusqlite::Connection, id: &SpaceId) -> Result<Option<Workspace>> {
    let query = format!(
        "SELECT {} FROM spaces JOIN workspaces USING(id) WHERE id=?1",
        store::WORKSPACE_COLUMNS
    );
    let workspace = db.query_row(&query, [id], store::workspace).optional()?;
    let Some(mut workspace) = workspace else {
        return Ok(None);
    };
    load_details(db, &mut workspace)?;
    Ok(Some(workspace))
}

fn load_details(db: &rusqlite::Connection, workspace: &mut Workspace) -> Result<()> {
    workspace.holds = super::holds::list(db, &workspace.id)?;
    workspace.review = super::review::list(db, &workspace.id)?;
    workspace.latest_review = crate::daemon::reviews::store::latest_status(db, &workspace.id)?;
    workspace.agent_state = super::agent_state::current(db, &workspace.id)?;
    workspace.progress = crate::daemon::progress::current(db, &workspace.id)?;
    workspace.sleep = crate::daemon::idle_wakes::sleep::current(db, &workspace.id)?;
    workspace.wake_at = crate::daemon::wake_timers::due_at(db, &workspace.id)?;
    workspace.asleep = super::agent_queue::asleep::current(db, &workspace.id)?;
    workspace.restart = crate::daemon::restarts::record::current(db, &workspace.id)?;
    stack::load(db, workspace)?;
    copies::load(db, workspace)?;
    super::tree::load(db, workspace)?;
    workspace.links = store::space_links(db, &workspace.id)?;
    workspace.running = store::exists(
        db,
        "SELECT 1 FROM executions WHERE space_id=?1 AND state=?2",
        rusqlite::params![workspace.id, crate::state::ExecutionState::Running],
    )?;
    workspace.done = store::exists(
        db,
        "SELECT 1 FROM space_completion WHERE space_id=?1",
        [&workspace.id],
    )?;
    workspace.pending_worktree = store::exists(
        db,
        "SELECT 1 FROM pending_worktrees WHERE workspace_id=?1",
        [&workspace.id],
    )?;
    workspace.preparation_waits =
        crate::daemon::operations::starts::preparation_waits(db, &workspace.id)?;
    Ok(())
}

#[cfg(test)]
mod tests;
