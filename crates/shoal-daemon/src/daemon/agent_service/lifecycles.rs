//! The lifecycles of workspaces, which an execution waits out and holds.
use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_agents::daemon::Lifecycles;

use crate::daemon::manager::Manager;
use crate::model::OpenedWorkspace;
use crate::model::Workspace;
use crate::state::WorkspaceState;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;
use shoal_store::lifecycle::Generation;

impl Lifecycles for Manager {
    fn set_state<'a>(
        &'a self,
        id: &'a SpaceId,
        state: WorkspaceState,
        error: Option<String>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.spaces.set_state(id, state, error))
    }

    fn reserve_lifecycle<'a>(
        &'a self,
        id: &'a SpaceId,
        state: WorkspaceState,
    ) -> BoxFuture<'a, Result<WorkspaceState>> {
        Box::pin(self.spaces.reserve_lifecycle(id, state))
    }

    fn reserved_since<'a>(
        &'a self,
        id: &'a SpaceId,
        seen: Option<Generation>,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(self.spaces.reserved_since(id, seen))
    }

    fn lifecycle_generation<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Generation>> {
        Box::pin(self.spaces.lifecycle_generation(id))
    }

    fn check_lifecycle<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.spaces.check_lifecycle(id))
    }

    fn await_reservation_end<'a>(
        &'a self,
        id: &'a SpaceId,
        wanted: &'a (dyn Fn() -> Result<()> + Send + Sync),
    ) -> BoxFuture<'a, Result<Generation>> {
        Box::pin(self.spaces.await_reservation_end(id, wanted))
    }

    fn ensure_no_pending_rename<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.spaces.ensure_no_pending_rename(id))
    }

    fn verify_worktree<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.spaces.verify_worktree(self, workspace))
    }

    fn reopen_workspace<'a>(
        &'a self,
        selector: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<OpenedWorkspace>> {
        Box::pin(self.spaces.reopen_workspace(self, selector))
    }

    fn refresh_admitted<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, ()> {
        Box::pin(self.spaces.refresh_admitted(self, workspace))
    }
}
