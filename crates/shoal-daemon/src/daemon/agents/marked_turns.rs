//! The turn an agent marks, as one whose final message goes out once it
//! ends: the turn of the execution it runs as, a chat's as a workspace's.
use anyhow::{Context, Result};
use shoal_ids::execution_ids::ExecutionId;

use super::Manager;
use crate::model::Space;

impl Manager {
    /// The execution `execution` whose turn an agent of `space` marks, with
    /// that agent, refusing an execution that runs no agent of the space.
    pub(crate) async fn agent_turn(
        &self,
        space: &Space,
        execution: Option<&ExecutionId>,
    ) -> Result<(ExecutionId, crate::agent::Agent)> {
        let refusal = || {
            format!(
                "only an agent Shoal runs in {} writes in its chat",
                space.name
            )
        };
        let execution = execution.with_context(refusal)?.to_owned();
        let agent = self
            .executions
            .with_agent(&execution, |agent| {
                (agent.workspace_id == space.id).then(|| agent.name.clone())
            })
            .await
            .flatten()
            .with_context(refusal)?;
        Ok((execution, agent))
    }
}
