//! The operations that start and prepare spaces.
use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_agents::daemon::Operations;

use crate::daemon::manager::Manager;
use crate::model::Space;
use shoal_ids::space_ids::SpaceId;
use std::{collections::HashSet, sync::Arc};

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

    fn pushing_operations(&self) -> Box<dyn Fn(&str) -> bool + Send + Sync> {
        let operations = self.operations.clone();
        Box::new(move |operation: &str| operations.runs(operation))
    }

    fn operations_work_in(&self) -> HashSet<SpaceId> {
        self.operations.working_in()
    }

    fn operation_runs_in(&self, id: &SpaceId) -> bool {
        self.operation_runs_in(id)
    }

    fn fakes_workers(&self) -> bool {
        crate::daemon::fake_workers::faked()
    }
}
