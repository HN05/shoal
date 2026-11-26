//! The demos the spaces' workspaces run.
use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_protocol::space_names::SpaceSelector;

use crate::daemon::manager::Manager;

impl shoal_spaces::daemon::Demos for Manager {
    fn stop_demos<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        name: Option<String>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.demos.stop(self, selector, name))
    }
}
