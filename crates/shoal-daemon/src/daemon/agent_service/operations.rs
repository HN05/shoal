//! The operations that start and prepare spaces.
use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_agents::daemon::Operations;

use crate::daemon::manager::Manager;
use crate::model::Space;
use std::sync::Arc;

impl Operations for Manager {
    fn start_waiting_preparations(self: Arc<Self>) -> BoxFuture<'static, Result<()>> {
        Box::pin(async move {
            let daemon = self.daemon();
            self.spaces.start_waiting_preparations(&daemon).await
        })
    }

    fn tell_start_warnings<'a>(
        &'a self,
        space: &'a Space,
        warnings: &'a [String],
    ) -> BoxFuture<'a, ()> {
        Box::pin(self.spaces.tell_start_warnings(self, space, warnings))
    }
}
