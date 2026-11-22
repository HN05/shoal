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
        Box::pin(self.set_state(id, state, error))
    }

    fn reserve_lifecycle<'a>(
        &'a self,
        id: &'a SpaceId,
        state: WorkspaceState,
    ) -> BoxFuture<'a, Result<WorkspaceState>> {
        Box::pin(self.reserve_lifecycle(id, state))
    }

    fn reserved_since<'a>(
        &'a self,
        id: &'a SpaceId,
        seen: Option<Generation>,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(self.reserved_since(id, seen))
    }

    fn lifecycle_generation<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Generation>> {
        Box::pin(self.lifecycle_generation(id))
    }

    fn check_lifecycle<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.check_lifecycle(id))
    }

    fn await_reservation_end<'a>(
        &'a self,
        id: &'a SpaceId,
        wanted: &'a (dyn Fn() -> Result<()> + Send + Sync),
    ) -> BoxFuture<'a, Result<Generation>> {
        Box::pin(self.await_reservation_end(id, wanted))
    }

    fn ensure_no_pending_rename<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.ensure_no_pending_rename(id))
    }

    fn verify_worktree<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.verify_worktree(workspace))
    }

    fn reopen_workspace<'a>(
        &'a self,
        selector: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<OpenedWorkspace>> {
        Box::pin(self.reopen_workspace(selector))
    }

    fn refresh_admitted<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, ()> {
        Box::pin(self.refresh_admitted(workspace))
    }
}
