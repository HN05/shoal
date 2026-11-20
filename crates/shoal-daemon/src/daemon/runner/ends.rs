//! The load of the runner's jobs as admission counts it, and the process
//! proof of whether a job's execution still runs.
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use std::collections::HashSet;

use anyhow::Result;

use crate::{
    daemon::{capacity::Activity, manager::Manager},
    process::{execution::Processes, identity},
    state::ExecutionState,
};

impl Manager {
    /// Whether the execution `id` of a job in `workspace_id` may still run.
    /// One whose wrapper has not reattached since a daemon restart, which
    /// restart recovery leaves to its owner since a job's runner has no agent
    /// record, counts as ended, and is forgotten, once the shared process
    /// proof finds nothing of it: no wrapper, and no process carrying its
    /// marker or in its process group. Verified survivors are stopped and
    /// checked again on the next pass; a launch whose identity was never
    /// recorded proves nothing and keeps the job.
    pub(super) async fn job_execution_runs(
        &self,
        workspace_id: &SpaceId,
        id: &ExecutionId,
    ) -> Result<bool> {
        let workspace = self.workspace_by_id(workspace_id).await?;
        let execution = self
            .workspace_executions(workspace_id)
            .await?
            .into_iter()
            .find(|execution| execution.id == *id);
        let Some(execution) = execution else {
            return Ok(false);
        };
        if execution.state != ExecutionState::Unknown {
            return Ok(true);
        }
        let scan = identity::scan(HashSet::from([id.to_owned()])).await?;
        let agents = self.agent_wrappers().await?;
        let processes = Processes::inspect(&execution, &workspace.path, &scan, &agents).await?;
        if processes.wrapper.is_some() || !processes.visibility_complete() {
            return Ok(true);
        }
        if !processes.command_stopped() {
            processes.stop_owned().await?;
            return Ok(true);
        }
        self.forget_execution(id).await?;
        Ok(false)
    }

    /// The load of each job starting or running, as admission counts it.
    pub(crate) async fn job_activities(&self) -> Result<Vec<Activity>> {
        let loads = self.runner.job_loads(self).await?;
        Ok((loads.into_iter())
            .map(|load| Activity::Job {
                age: load.age,
                cost: load.cost,
                memory: load.memory,
            })
            .collect())
    }
}
