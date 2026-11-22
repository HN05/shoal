//! The executions that run agents.
use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_agents::daemon::Executions;
use shoal_ids::{execution_ids::ExecutionId, space_ids::SpaceId};

use crate::{agent::Agent, daemon::manager::Manager, process::identity::AgentTrees};

impl Executions for Manager {
    fn agent_name<'a>(&'a self, id: &'a ExecutionId) -> BoxFuture<'a, Option<Agent>> {
        Box::pin(self.agent_name(id))
    }

    fn agent_stopping<'a>(&'a self, id: &'a ExecutionId) -> BoxFuture<'a, bool> {
        Box::pin(self.agent_stopping(id))
    }

    fn agent_trees(&self) -> BoxFuture<'_, Result<AgentTrees>> {
        Box::pin(self.agent_trees())
    }

    fn running_agent_spaces(&self) -> BoxFuture<'_, Vec<SpaceId>> {
        Box::pin(self.running_agent_workspaces())
    }
}
