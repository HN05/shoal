//! The spaces the runner's jobs are for and run in.

use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;
use shoal_runner::daemon::Spaces;

use crate::{
    daemon::manager::Manager,
    model::{Space, Workspace, WorkspaceUsage},
};

impl Spaces for Manager {
    fn present_spaces(&self) -> BoxFuture<'_, Result<Vec<Space>>> {
        Box::pin(Manager::present_spaces(self))
    }

    fn workspace_usage(&self, id: &SpaceId) -> Option<WorkspaceUsage> {
        Manager::workspace_usage(self, id)
    }

    fn workspace_by_id<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Workspace>> {
        Box::pin(Manager::workspace_by_id(self, id))
    }

    fn select_space_id<'a>(
        &'a self,
        selector: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<SpaceId>> {
        Box::pin(async move { Ok(self.space(selector).await?.id) })
    }
}
