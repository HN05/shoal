//! The workspace records the daemon reads.
mod adoption;
mod availability;
mod branch_rename;
pub mod branches;
pub mod checkpoints;
pub mod commits;
pub mod conflicts;
pub mod converted;
pub mod copies;
pub mod creation;
mod diff;
mod disowned;
pub mod fast_forward;
pub mod kept_branches;
pub mod location;
pub mod merge;
pub mod ownership;
pub mod refresh;
pub mod reservation;
pub mod stack;
pub mod status;
pub mod usage;
pub mod xcode;

use crate::Spaces;
use crate::daemon::{Daemon, Records};
pub use branches::NewWorktreeOptions;
pub use copies::CopyPlace;
pub use creation::{NewWorkspace, build_pending_in, pending_revisions};
pub use merge::squash_message;
pub use ownership::VerifiedWorktree;
pub use stack::record_base_changed;

use crate::{
    model::{Execution, Workspace},
    {spaces::status::SpaceRecords, store},
};
use anyhow::Result;
use rusqlite::OptionalExtension;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::status::Inspection;

impl Spaces {
    pub async fn list_workspaces(&self) -> Result<Vec<Workspace>> {
        self.list_workspaces_within(None).await
    }

    /// Every workspace without `root`. By name. Only the selected workspaces
    /// load their details.
    pub async fn list_workspaces_within(&self, root: Option<&SpaceId>) -> Result<Vec<Workspace>> {
        let (root, records) = (root.cloned(), self.records.clone());
        self.store
            .run(move |db| workspaces_within(db, &*records, root.as_ref()))
            .await
    }

    /// From the latest sample.
    pub fn workspace_usage(&self, id: &SpaceId) -> Option<crate::model::WorkspaceUsage> {
        let now = tokio::time::Instant::now();
        self.usage.with_workspaces(|usages| usages.get(id, now))
    }

    /// In any state.
    pub async fn workspace_has_executions(&self, id: &SpaceId) -> Result<bool> {
        let id = id.to_owned();
        self.store
            .run(move |db| Ok(crate::executions::execution_count(db, &id, None)? > 0))
            .await
    }

    /// In every state.
    pub async fn workspace_executions(&self, id: &SpaceId) -> Result<Vec<Execution>> {
        let id = id.to_owned();
        self.store.run(move |db| store::executions(db, &id)).await
    }

    /// Counts without decoding the records.
    pub async fn workspace_execution_count(&self, id: &SpaceId) -> Result<usize> {
        let id = id.to_owned();
        self.store
            .run(move |db| crate::executions::execution_count(db, &id, None))
            .await
    }

    /// Reads in one store operation, so a removal meanwhile reads as none.
    pub async fn find_workspace(&self, id: &SpaceId) -> Result<Option<Workspace>> {
        let id = id.to_owned();
        let records = self.records.clone();
        self.store
            .run(move |db| recorded_workspace(db, &*records, &id))
            .await
    }

    pub async fn inspect_workspace(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
    ) -> Result<Inspection> {
        let workspace = self.workspace(selector).await?;
        let simulators = daemon.list_simulators(Some(&workspace.id)).await?;
        let SpaceRecords {
            issues,
            pr_cleanup,
            completion,
        } = self.space_records(daemon, &workspace.id).await?;
        let places = (daemon
            .owned_places(Some(workspace.id.clone()))
            .await?
            .into_iter())
        .map(|(_, place)| place)
        .collect();
        let selector = SpaceSelector::id(&workspace.id);
        let resources = daemon.list_resources(Some(&selector)).await?;
        let mut inspection = self
            .store
            .run(move |db| {
                Ok(Inspection {
                    issues,
                    completion,
                    pr_cleanup,
                    executions: store::executions(db, &workspace.id)?,
                    places,
                    ports: store::ports(db, Some(&workspace.id))?,
                    resources,
                    simulators,
                    workspace,
                    machine: None,
                })
            })
            .await?;
        daemon.annotate_stops(&mut inspection.executions).await;
        Ok(inspection)
    }

    /// Automatic cleanup restarts its idle timer, and the store keeps the
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
            crate::log!("activity time not recorded: {error:#}");
        }
    }

    /// A workspace without any starts its idle period now. A chat records no
    /// such time.
    pub async fn active_at(&self, id: &SpaceId) -> Result<Option<crate::time::UnixTime>> {
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

    /// Drops the in-memory bookkeeping of a workspace that no longer exists.
    pub async fn forget_workspace(&self, daemon: &dyn Daemon, id: &SpaceId) {
        self.usage.forget_workspace(id);
        daemon.forget_rejected_config(id);
        daemon.forget_workspace_executions(id).await;
    }
}

/// Every workspace without `root`, by name, each with its details.
pub fn workspaces_within(
    db: &rusqlite::Connection,
    records: &dyn Records,
    root: Option<&SpaceId>,
) -> Result<Vec<Workspace>> {
    let mut workspaces = db
        .prepare_cached(&super::tree::subtree_query(store::WORKSPACE_COLUMNS))?
        .query_map([root], store::workspace)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for workspace in &mut workspaces {
        load_details(db, records, workspace)?;
    }
    Ok(workspaces)
}

/// Whether the workspace `id` is recorded in `state`.
pub fn is_in_state(
    db: &rusqlite::Connection,
    id: &SpaceId,
    state: crate::state::WorkspaceState,
) -> Result<bool> {
    store::exists(
        db,
        "SELECT 1 FROM workspaces WHERE id=?1 AND state=?2",
        rusqlite::params![id, state],
    )
}

fn recorded_workspace(
    db: &rusqlite::Connection,
    records: &dyn Records,
    id: &SpaceId,
) -> Result<Option<Workspace>> {
    let query = format!(
        "SELECT {} FROM spaces JOIN workspaces USING(id) WHERE id=?1",
        store::WORKSPACE_COLUMNS
    );
    let workspace = db
        .prepare_cached(&query)?
        .query_row([id], store::workspace)
        .optional()?;
    let Some(mut workspace) = workspace else {
        return Ok(None);
    };
    load_details(db, records, &mut workspace)?;
    Ok(Some(workspace))
}

fn load_details(
    db: &rusqlite::Connection,
    records: &dyn Records,
    workspace: &mut Workspace,
) -> Result<()> {
    workspace.holds = super::holds::list(db, &workspace.id)?;
    workspace.review = super::ready_marks::list(db, &workspace.id)?;
    workspace.progress = crate::progress::current(db, &workspace.id)?;
    stack::load(db, workspace)?;
    copies::load(db, workspace)?;
    super::tree::load(db, workspace)?;
    let running = Some(crate::state::ExecutionState::Running);
    workspace.running = crate::executions::execution_count(db, &workspace.id, running)? > 0;
    workspace.done = store::spaces::done(db, &workspace.id)?;
    workspace.preparation_waits = crate::starts::preparation_waits(db, &workspace.id)?;
    records.load_workspace(db, workspace)
}

#[cfg(test)]
mod tests;
