//! Workspace lifecycle reservations: a transient state that excludes every
//! other lifecycle operation, the state it leaves the workspace in, and
//! waiting for such a state to pass.

use super::super::gates::GuardMode;
use crate::Spaces;
use crate::{state::WorkspaceState, store};
use anyhow::{Result, bail};
use rusqlite::{OptionalExtension, params};
use shoal_ids::space_ids::SpaceId;

pub use shoal_store::lifecycle::Generation;
use shoal_store::lifecycle::{Lifecycle, lifecycle};

impl Spaces {
    pub async fn set_state(
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

    pub async fn set_state_if(
        &self,
        id: &SpaceId,
        from: WorkspaceState,
        state: WorkspaceState,
        error: Option<String>,
    ) -> Result<()> {
        let id = id.to_owned();
        self.store
            .write(move |tx| {
                tx.execute(
                    "UPDATE workspaces SET state=?2, error=?3 WHERE id=?1 AND state=?4",
                    params![id, state, error, from],
                )?;
                Ok(())
            })
            .await
    }

    pub async fn lifecycle_generation(&self, id: &SpaceId) -> Result<Generation> {
        match self.lifecycle(id).await? {
            Some(lifecycle) => Ok(lifecycle.generation),
            None => bail!("workspace no longer exists"),
        }
    }

    /// Also true when the lifecycle changed since `seen`, as an operation
    /// that came and went changes it. A chat has none.
    pub async fn reserved_since(&self, id: &SpaceId, seen: Option<Generation>) -> Result<bool> {
        Ok(match self.lifecycle(id).await? {
            Some(Lifecycle { state, generation }) => state.passes() || seen != Some(generation),
            None => seen.is_some(),
        })
    }

    /// Gives up with the error `wanted` returns, which it asks each time it
    /// looks. Returns the generation the workspace then has.
    pub async fn await_reservation_end(
        &self,
        id: &SpaceId,
        wanted: impl Fn() -> Result<()>,
    ) -> Result<Generation> {
        // Subscribe before reading, so a change in between still wakes.
        let mut changed = self.store.watch_changes();
        loop {
            wanted()?;
            match self.lifecycle(id).await? {
                None => bail!("workspace no longer exists"),
                Some(lifecycle) if lifecycle.state.passes() => changed.changed().await?,
                Some(lifecycle) => return Ok(lifecycle.generation),
            }
        }
    }

    async fn lifecycle(&self, id: &SpaceId) -> Result<Option<Lifecycle>> {
        let id = id.to_owned();
        self.store.run(move |db| lifecycle(db, &id)).await
    }

    /// Excludes every other lifecycle operation until the state is restored.
    pub async fn reserve_lifecycle(
        &self,
        id: &SpaceId,
        state: WorkspaceState,
    ) -> Result<WorkspaceState> {
        self.reserve_lifecycle_inner(id, Some(state), RemovalRules::default())
            .await
    }

    /// Checks without reserving, so a batch can check every target before
    /// acting on any.
    pub async fn check_lifecycle(&self, id: &SpaceId) -> Result<()> {
        self.reserve_lifecycle_inner(id, None, RemovalRules::default())
            .await?;
        Ok(())
    }

    pub async fn reserve_lifecycle_for_removal(
        &self,
        id: &SpaceId,
        rules: RemovalRules,
    ) -> Result<WorkspaceState> {
        self.reserve_lifecycle_inner(id, Some(WorkspaceState::Removing), rules)
            .await
    }

    /// Only checks when `state` is `None`.
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
                // Removal checks pending intent after reserving the
                // lifecycle, so a confirmed-deleted worktree can release its
                // ownership.
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
                        if rules.awaiting_setup && crate::executions::execution_count(tx, &id, None)? == 0 =>
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

/// What reserving for removal accepts besides a ready or failed workspace.
#[derive(Clone, Copy, Default)]
pub struct RemovalRules {
    /// A pending rename, which removing a deleted worktree releases.
    pub allow_pending_rename: bool,
    /// A preparing workspace whose setup never started. The caller waited
    /// for its creation to end and holds the repository's Git gate, under
    /// which setup starts.
    pub awaiting_setup: bool,
}
