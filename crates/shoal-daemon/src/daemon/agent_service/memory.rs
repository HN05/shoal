//! The memory agents' work takes.
use futures_util::future::BoxFuture;
use shoal_agents::daemon::Memory;

use crate::daemon::manager::Manager;
use crate::process::load::WorkLimit;
use crate::protocol::ScopeMemory;
use shoal_ids::execution_ids::ExecutionId;

impl Memory for Manager {
    fn record_scope_memory<'a>(
        &'a self,
        id: &'a ExecutionId,
        memory: ScopeMemory,
    ) -> BoxFuture<'a, ()> {
        Box::pin(self.record_scope_memory(id, memory))
    }

    fn work_limit(&self) -> Option<WorkLimit> {
        self.work_limit()
    }
}
