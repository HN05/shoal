//! Workspace lifecycle reservation: holding a workspace in a transient
//! state that excludes every other lifecycle operation, and recording the
//! state it leaves it in, and waiting for such a state to pass or telling
//! that one came and went.

use super::super::{gates::GuardMode, manager::Manager};
use crate::{daemon::store, state::WorkspaceState};
use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use shoal_ids::space_ids::SpaceId;

/// The refusal of a workspace held in a lifecycle state that passes, which
/// [`Manager::await_reservation_end`] waits out.
#[derive(Debug)]
pub(crate) struct Reserved(WorkspaceState);

impl std::fmt::Display for Reserved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "workspace is {}", self.0)
    }
}

impl std::error::Error for Reserved {}

/// How many times a workspace's lifecycle state has changed, which the
/// store counts on each change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Generation(i64);

/// Where a workspace's lifecycle stands.
struct Lifecycle {
    state: WorkspaceState,
    generation: Generation,
}

/// Refuse the workspace with [`Reserved`] while a lifecycle operation that
/// passes holds it.
pub(crate) fn ensure_unreserved(db: &Connection, id: &SpaceId) -> Result<()> {
    match lifecycle(db, id)? {
        Some(Lifecycle { state, .. }) if state.passes() => Err(Reserved(state).into()),
        _ => Ok(()),
    }
}

fn lifecycle(db: &Connection, id: &SpaceId) -> Result<Option<Lifecycle>> {
    Ok(db
        .query_row(
            "SELECT state, lifecycle_generation FROM workspaces WHERE id=?1",
            [id],
            |row| {
                Ok(Lifecycle {
                    state: row.get(0)?,
                    generation: Generation(row.get(1)?),
                })
            },
        )
        .optional()?)
}

impl Manager {
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

    /// [`Self::set_state`] while the workspace is still in the state `from`.
    pub(crate) async fn set_state_if(
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

    /// The lifecycle generation of the workspace `id`.
    pub(crate) async fn lifecycle_generation(&self, id: &SpaceId) -> Result<Generation> {
        match self.lifecycle(id).await? {
            Some(lifecycle) => Ok(lifecycle.generation),
            None => bail!("workspace no longer exists"),
        }
    }

    /// Whether a lifecycle operation that passes holds the space `id` now,
    /// or its lifecycle changed since it stood at `seen`, as one that came
    /// and went changes it; a chat has none.
    pub(crate) async fn reserved_since(
        &self,
        id: &SpaceId,
        seen: Option<Generation>,
    ) -> Result<bool> {
        Ok(match self.lifecycle(id).await? {
            Some(Lifecycle { state, generation }) => state.passes() || seen != Some(generation),
            None => seen.is_some(),
        })
    }

    /// Wait until no lifecycle operation that passes holds the workspace,
    /// giving up with the error `wanted` returns, which it asks each time
    /// it looks, and return the generation it then has.
    pub(crate) async fn await_reservation_end(
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

    pub(in crate::daemon) async fn reserve_lifecycle_for_removal(
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
                                "SELECT 1 FROM executions WHERE space_id=?1",
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
pub(in crate::daemon) struct RemovalRules {
    /// A pending rename, which removing a deleted worktree releases.
    pub(in crate::daemon) allow_pending_rename: bool,
    /// A preparing workspace whose setup never started; the caller waited
    /// for its creation to end and holds the repository's Git gate, under
    /// which setup starts.
    pub(in crate::daemon) awaiting_setup: bool,
}
