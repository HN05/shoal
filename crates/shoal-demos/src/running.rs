//! The demos running now: listing them, how far a start got, stopping them
//! and running a demo's command again.
use std::collections::HashSet;

use anyhow::{Result, ensure};
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::demos::{Progress, Running, validate_name};
use shoal_protocol::space_names::SpaceSelector;

use crate::{
    Demos,
    daemon::{Daemon, Executions, Proof, Spaces},
    model::Execution,
    process::identity as process,
    records,
};

impl Demos {
    /// The running demos of `selector`'s workspace, none of a chat's, or
    /// those of every workspace.
    pub async fn list(
        &self,
        daemon: &dyn Spaces,
        selector: Option<&SpaceSelector>,
    ) -> Result<Vec<Running>> {
        let workspace = match selector {
            Some(selector) => {
                let space = daemon.space(selector).await?;
                if space.worktree().is_none() {
                    return Ok(Vec::new());
                }
                Some(space.id)
            }
            None => None,
        };
        let recorded = self
            .store
            .run(move |db| records::list(db, workspace.as_ref()))
            .await?;
        let mut demos = Vec::new();
        for demo in recorded {
            // Its record goes with its workspace, which a removal may
            // delete meanwhile.
            if let Some(space) = daemon.find_space(&demo.workspace_id).await? {
                demos.push(demo.running(space.name));
            }
        }
        demos.sort_by(|a, b| (&a.workspace, &a.launch.name).cmp(&(&b.workspace, &b.launch.name)));
        for demo in &mut demos {
            let viewer = self.running_viewer(&demo.execution_id).await?;
            let serving = viewer.filter(|viewer| viewer.serving);
            demo.viewer = demo.launch.viewer(serving.map(|viewer| viewer.port));
        }
        Ok(demos)
    }

    /// How far the start of `launch` in `selector`'s workspace got.
    pub async fn progress(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        launch: &str,
    ) -> Result<Progress> {
        let recorded = self
            .list(daemon, Some(selector))
            .await?
            .into_iter()
            .find(|demo| demo.launch.id == launch);
        Ok(match recorded {
            Some(demo) if demo.has_run => Progress::Running {
                demo: Box::new(demo),
            },
            Some(_) => Progress::Starting,
            None if daemon.launch_ended_unrun(launch) => Progress::Ended,
            None => Progress::Starting,
        })
    }

    /// The launch of the demo `execution` runs, while its command has not
    /// started; read before the execution finishes, which deletes it.
    pub async fn unrun_launch(&self, execution: &ExecutionId) -> Result<Option<String>> {
        let execution = execution.to_owned();
        (self.store)
            .run(move |db| records::unrun_launch(db, &execution))
            .await
    }

    /// Ask the space's demo `name`, or every demo there, to stop. Their
    /// records go once their wrappers finish; a demo whose wrapper has not
    /// reconnected since a daemon restart has its verified processes stopped
    /// as `shoal stop` stops them.
    pub async fn stop(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        name: Option<String>,
    ) -> Result<()> {
        if let Some(name) = &name {
            validate_name(name)?;
        }
        let space = daemon.space(selector).await?.id;
        let executions: Vec<ExecutionId> = self
            .list(daemon, Some(&SpaceSelector::id(&space)))
            .await?
            .into_iter()
            .filter(|demo| name.as_ref().is_none_or(|name| *name == demo.launch.name))
            .map(|demo| demo.execution_id)
            .collect();
        let reason = "stopped by shoal demo stop".to_owned();
        self.stop_executions(daemon, executions, reason).await
    }

    /// Ask every demo of this machine to stop for `reason`, as
    /// [`Self::stop`] stops a space's.
    pub async fn stop_all(&self, daemon: &dyn Daemon, reason: String) -> Result<()> {
        let demos = self.list(daemon, None).await?;
        let executions = demos.into_iter().map(|demo| demo.execution_id).collect();
        self.stop_executions(daemon, executions, reason).await
    }

    /// Ask the demos `executions` run to stop for `reason`, stopping the
    /// verified processes of those whose wrappers are away.
    async fn stop_executions(
        &self,
        daemon: &dyn Daemon,
        executions: Vec<ExecutionId>,
        reason: String,
    ) -> Result<()> {
        let disconnected = daemon.ask_to_stop(executions, reason).await;
        for id in disconnected {
            if let Some(execution) = daemon.execution(&id).await? {
                self.stop_disconnected(daemon, &execution).await?;
            }
        }
        Ok(())
    }

    /// Stop a demo whose wrapper is away. With no child recorded, before its
    /// first run registers or once the last run's stop was proven, nothing
    /// names a process group, so the proof is that nothing carrying the
    /// execution's marker survives and every live process could be read.
    async fn stop_disconnected(
        &self,
        daemon: &dyn Executions,
        execution: &Execution,
    ) -> Result<()> {
        let between_runs = execution.child.is_none() && execution.group_id.is_none();
        if !between_runs {
            return daemon.stop_disconnected(execution, Proof::Complete).await;
        }
        daemon
            .stop_disconnected(execution, Proof::ForRemoval)
            .await?;
        let scan = process::scan(HashSet::from([execution.id.clone()])).await?;
        ensure!(
            daemon
                .inspect_execution(execution, &scan)
                .await?
                .command_stopped(),
            "process state of the demo remains uncertain; check its processes, then stop it again"
        );
        daemon.forget_execution(&execution.id).await
    }

    /// Stop the verified processes of the demo `execution` runs, whose
    /// wrapper runs its command again, freeing what it held, short of its
    /// viewer, which outlives the demo's runs; returns whether any ran.
    pub async fn restart_command(
        &self,
        daemon: &dyn Executions,
        execution: &ExecutionId,
    ) -> Result<bool> {
        // A viewer spawning meanwhile is spared once recorded.
        let spawning = self.viewers.spawn_lock(execution);
        let _spawning = spawning.lock().await;
        let Some(record) = daemon.execution(execution).await? else {
            return Ok(false);
        };
        let scan = process::scan(HashSet::from([record.id.clone()])).await?;
        let mut processes = daemon.inspect_execution(&record, &scan).await?;
        if let Some(viewer) = self.running_viewer(execution).await? {
            processes.spare_group(viewer.process.pid);
        }
        processes.stop_owned().await?;
        Ok(!processes.owned.is_empty())
    }
}
