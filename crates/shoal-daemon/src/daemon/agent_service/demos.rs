//! The demos executions run and the viewers of their simulators.
use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_agents::daemon::Demos;

use crate::daemon::manager::Manager;
use shoal_ids::execution_ids::ExecutionId;
use shoal_schedule::stopping::RunningDemo;
use std::sync::Arc;

impl Demos for Manager {
    fn running_demos(&self) -> BoxFuture<'_, Result<Vec<RunningDemo>>> {
        Box::pin(async {
            let demos = self.demos.list(self, None).await?;
            Ok((demos.into_iter())
                .map(|demo| RunningDemo {
                    execution: demo.execution_id,
                    space: demo.workspace_id,
                    started: demo.started_at,
                })
                .collect())
        })
    }

    fn viewer_group<'a>(
        &'a self,
        execution: &'a ExecutionId,
    ) -> BoxFuture<'a, Result<Option<u32>>> {
        Box::pin(async {
            let viewer = self.demos.running_viewer(execution).await?;
            Ok(viewer.map(|viewer| viewer.process.pid))
        })
    }

    fn retire_execution_viewer<'a>(
        &'a self,
        execution: &'a ExecutionId,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.demos.retire_execution_viewer(execution))
    }

    fn reported_end<'a>(
        &'a self,
        execution: &'a ExecutionId,
        exit_code: Option<i32>,
    ) -> BoxFuture<'a, Option<i32>> {
        Box::pin(self.demos.reported_end(execution, exit_code))
    }

    fn unrun_demo_launch<'a>(
        &'a self,
        execution: &'a ExecutionId,
    ) -> BoxFuture<'a, Result<Option<String>>> {
        Box::pin(self.demos.unrun_launch(execution))
    }

    fn restart_demo_command<'a>(
        &'a self,
        execution: &'a ExecutionId,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(self.demos.restart_command(self, execution))
    }

    fn viewer_spawn_lock(&self, execution: &ExecutionId) -> Arc<tokio::sync::Mutex<()>> {
        self.demos.viewer_spawn_lock(execution)
    }

    fn end_viewer(&self, execution: &ExecutionId) {
        self.demos.end_viewer(execution);
    }
}
