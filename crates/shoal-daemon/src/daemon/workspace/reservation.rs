//! Workspace lifecycle reservation: holding a workspace in a transient
//! state that excludes every other lifecycle operation, and recording the
//! state it leaves it in.

use super::{GuardMode, Manager};
use crate::client::space_ids::SpaceId;
use crate::{daemon::store, state::WorkspaceState};
use anyhow::{Result, bail, ensure};
use rusqlite::{OptionalExtension, params};

impl Manager {
    pub(crate) async fn set_state(
        &self,
        id: &SpaceId,
        state: WorkspaceState,
        error: Option<String>,
    ) -> Result<()> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE workspaces SET state=?2, error=?3 WHERE id=?1",
                    params![id, state, error],
                )?;
                Ok(())
            })
            .await
    }

    /// Move a ready or failed workspace into a transient lifecycle state,
    /// excluding every other lifecycle operation until it is restored.
    pub(crate) async fn reserve_lifecycle(
        &self,
        id: &SpaceId,
        state: WorkspaceState,
    ) -> Result<WorkspaceState> {
        self.reserve_lifecycle_inner(id, Some(state), RemovalRules::default())
            .await
    }

    /// Refuse what [`Self::reserve_lifecycle`] would refuse now, without
    /// reserving, so a batch can check every target before acting on any.
    pub(crate) async fn check_lifecycle(&self, id: &SpaceId) -> Result<()> {
        self.reserve_lifecycle_inner(id, None, RemovalRules::default())
            .await?;
        Ok(())
    }

    pub(super) async fn reserve_lifecycle_for_removal(
        &self,
        id: &SpaceId,
        rules: RemovalRules,
    ) -> Result<WorkspaceState> {
        self.reserve_lifecycle_inner(id, Some(WorkspaceState::Removing), rules)
            .await
    }

    /// Reserve the workspace in `state`, or only check that it could be
    /// without `state`.
    async fn reserve_lifecycle_inner(
        &self,
        id: &SpaceId,
        state: Option<WorkspaceState>,
        rules: RemovalRules,
    ) -> Result<WorkspaceState> {
        let _resources = self.gates.resource_guard(id, GuardMode::Shared)?;
        let id = id.to_owned();
        self.store
            .write(move |tx| {
                // Removal checks pending intent after reserving the lifecycle,
                // so a confirmed-deleted worktree can release its ownership.
                if state != Some(WorkspaceState::Reconciling) && !rules.allow_pending_rename {
                    store::ensure_no_pending_rename(tx, &id)?;
                }
                store::ensure_no_pending_move(tx, &id)?;
                let current: Option<WorkspaceState> = tx
                    .query_row("SELECT state FROM workspaces WHERE id=?1", [&id], |row| {
                        row.get(0)
                    })
                    .optional()?;
                let current = match current {
                    None => bail!("workspace no longer exists"),
                    Some(current @ (WorkspaceState::Ready | WorkspaceState::Failed)) => current,
                    // Setup registers its execution as it reserves the
                    // workspace, so none recorded means none started.
                    Some(current @ WorkspaceState::Preparing)
                        if rules.awaiting_setup
                            && !store::exists(
                                tx,
                                "SELECT 1 FROM executions WHERE workspace_id=?1",
                                [&id],
                            )? =>
                    {
                        current
                    }
                    Some(WorkspaceState::Removing) => {
                        bail!("workspace is already being removed; it will disappear shortly")
                    }
                    Some(current) => bail!(
                        "workspace is {current}; wait for that operation to finish before another lifecycle operation"
                    ),
                };
                // Holds need a ready workspace, so none can arrive once it is
                // reserved.
                ensure!(
                    !rules.refuse_holds
                        || !store::exists(
                            tx,
                            "SELECT 1 FROM workspace_holds WHERE workspace_id=?1 AND placed_by=?2",
                            params![id, crate::model::HoldPlacer::User],
                        )?,
                    "workspace has holds, which are the user's; ask the user to remove it"
                );
                if let Some(state) = state {
                    tx.execute(
                        "UPDATE workspaces SET state=?2 WHERE id=?1",
                        params![id, state],
                    )?;
                }
                Ok(current)
            })
            .await
    }
}

/// What reserving a workspace for removal accepts besides a ready or failed
/// workspace, and what it refuses.
#[derive(Clone, Copy, Default)]
pub(super) struct RemovalRules {
    /// A pending rename, which removing a deleted worktree releases.
    pub(super) allow_pending_rename: bool,
    /// A preparing workspace whose setup never started; the caller excludes
    /// its creation by holding the repository's Git gate.
    pub(super) awaiting_setup: bool,
    /// A workspace with holds the user placed, whose removal would release
    /// them.
    pub(super) refuse_holds: bool,
}
