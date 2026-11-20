//! The executions of the runner's jobs: launching a job's runner, the
//! process proof of whether it still runs, and stopping its processes.
use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_forge::ci::runner::JobCommand;
use shoal_ids::{execution_ids::ExecutionId, space_ids::SpaceId};
use shoal_protocol::space_names::SpaceSelector;

use crate::{
    daemon::{
        events::StoppedProcess,
        executions::{Save, StopRequest},
        manager::Manager,
        space_processes::Selection,
    },
    model::Workspace,
    process::{execution::Processes, identity},
    state::ExecutionState,
};

impl shoal_runner::daemon::Executions for Manager {
    fn stop_job_processes<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Vec<StoppedProcess>>> {
        Box::pin(async move {
            let workspace = self.workspace_by_id(id).await?;
            (self.stop_workspace_untracked(&workspace, Selection::Processes, None)).await
        })
    }

    fn interrupt_job_processes<'a>(
        &'a self,
        id: &'a SpaceId,
        first: BoxFuture<'a, Result<()>>,
    ) -> BoxFuture<'a, Result<Vec<StoppedProcess>>> {
        Box::pin(async move {
            let workspace = self.workspace_by_id(id).await?;
            let processes = Selection::Processes;
            (self.stop_workspace_untracked_after(&workspace, processes, None, first)).await
        })
    }

    fn execution_recorded<'a>(&'a self, id: &'a ExecutionId) -> BoxFuture<'a, Result<bool>> {
        let id = id.to_owned();
        Box::pin(self.store.run(move |db| {
            crate::daemon::store::exists(db, "SELECT 1 FROM executions WHERE id=?1", [id])
        }))
    }

    fn workspace_has_executions<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<bool>> {
        Box::pin(Manager::workspace_has_executions(self, id))
    }

    fn job_execution_runs<'a>(
        &'a self,
        workspace: &'a SpaceId,
        id: &'a ExecutionId,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(Manager::job_execution_runs(self, workspace, id))
    }

    fn workspace_execution<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Option<ExecutionId>>> {
        Box::pin(async move {
            let executions = self.workspace_executions(id).await?;
            Ok(executions.into_iter().next().map(|execution| execution.id))
        })
    }

    fn launch_job_command<'a>(
        &'a self,
        workspace: &'a Workspace,
        log: std::path::PathBuf,
        command: shoal_forge::ci::runner::JobCommand,
    ) -> BoxFuture<'a, Result<ExecutionId>> {
        Box::pin(Manager::launch_job_command(self, workspace, log, command))
    }

    fn stop_job_runner<'a>(&'a self, id: &'a SpaceId, reason: String) -> BoxFuture<'a, Result<()>> {
        let request = StopRequest {
            reason,
            save: Save::Nothing,
        };
        Box::pin(async move { self.stop_space(&SpaceSelector::id(id), request).await })
    }
}

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
}

impl Manager {
    /// Run `command` in `workspace` as a tracked execution that outlives the
    /// daemon, and the service it runs as, with its output in `log`,
    /// returning the execution's ID.
    #[cfg(not(test))]
    pub(super) async fn launch_job_command(
        &self,
        workspace: &Workspace,
        log: PathBuf,
        command: JobCommand,
    ) -> Result<ExecutionId> {
        let env: Vec<(&str, String)> = (command.env.iter())
            .map(|(name, value)| (name.as_str(), value.clone()))
            .collect();
        let wrapper = crate::execution::Wrapper {
            env: &env,
            ..Default::default()
        };
        let execution = crate::execution::launch_detached(
            &self.paths,
            &workspace.into(),
            log,
            command.argv,
            &wrapper,
            None,
        )
        .await?;
        Ok(execution.launch.execution_id)
    }

    /// Tests run no command; they record it with an execution they end.
    #[cfg(test)]
    pub(super) async fn launch_job_command(
        &self,
        workspace: &Workspace,
        _log: PathBuf,
        command: JobCommand,
    ) -> Result<ExecutionId> {
        super::tests::launch(self, workspace, command).await
    }
}
