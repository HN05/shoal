//! Demos the daemon records with the executions that run them, so a demo
//! ends with its execution: when it stops, its workspace is removed or its
//! wrapper finishes.
use std::collections::HashSet;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::{
    Manager,
    executions::{Save, StopPolicy, StopRequest},
};
use crate::{
    demos::{Launch, Progress, Running},
    model::Execution,
    process::identity as process,
};

impl Manager {
    /// The running demos of `selector`'s workspace, or of every workspace.
    pub async fn demos(&self, selector: Option<&str>) -> Result<Vec<Running>> {
        let workspace = match selector {
            Some(selector) => Some(self.workspace(selector).await?.id),
            None => None,
        };
        self.store
            .run(move |db| list(db, workspace.as_deref()))
            .await
    }

    /// How far the start of `launch` in `selector`'s workspace got.
    pub async fn demo_progress(&self, selector: &str, launch: &str) -> Result<Progress> {
        let recorded = self
            .demos(Some(selector))
            .await?
            .into_iter()
            .find(|demo| demo.launch.id == launch);
        Ok(match recorded {
            Some(demo) if demo.has_run => Progress::Running {
                demo: Box::new(demo),
            },
            Some(_) => Progress::Starting,
            None if self.executions.launch_ended_unrun(launch) => Progress::Ended,
            None => Progress::Starting,
        })
    }

    /// The launch of the demo `execution` runs, while its command has not
    /// started; read before the execution finishes, which deletes it.
    pub(crate) async fn unrun_demo_launch(&self, execution: &str) -> Result<Option<String>> {
        let execution = execution.to_owned();
        self.store
            .run(move |db| {
                let record: Option<String> = db
                    .query_row(
                        "SELECT record FROM demos WHERE execution_id=?1 AND has_run=0",
                        [execution],
                        |row| row.get(0),
                    )
                    .optional()?;
                record
                    .map(|record| {
                        serde_json::from_str::<Launch>(&record)
                            .map(|launch| launch.id)
                            .context("invalid demo record")
                    })
                    .transpose()
            })
            .await
    }

    /// Ask the workspace's demo `name`, or every demo there, to stop. Their
    /// records go once their wrappers finish; a demo whose wrapper has not
    /// reconnected since a daemon restart has its verified processes stopped
    /// as `shoal stop` stops them.
    pub async fn stop_demos(&self, selector: &str, name: Option<String>) -> Result<()> {
        if let Some(name) = &name {
            crate::demos::validate_name(name)?;
        }
        let workspace = self.workspace(selector).await?.id;
        let executions: Vec<String> = self
            .demos(Some(&workspace))
            .await?
            .into_iter()
            .filter(|demo| name.as_ref().is_none_or(|name| *name == demo.launch.name))
            .map(|demo| demo.execution_id)
            .collect();
        let disconnected = self.request_demo_stops(executions).await;
        for id in disconnected {
            let query = id.clone();
            let execution = self
                .store
                .run(move |db| crate::daemon::store::find_execution(db, &query))
                .await?;
            if let Some(execution) = execution {
                self.stop_disconnected_demo(&execution).await?;
            }
        }
        Ok(())
    }

    /// Stop a demo whose wrapper is away. With no child recorded, before its
    /// first run registers or once the last run's stop was proven, nothing
    /// names a process group, so the proof is that nothing carrying the
    /// execution's marker survives and every live process could be read.
    async fn stop_disconnected_demo(&self, execution: &Execution) -> Result<()> {
        let between_runs = execution.child.is_none() && execution.group_id.is_none();
        if !between_runs {
            return self
                .stop_disconnected(execution, StopPolicy::RequireCompleteProof)
                .await;
        }
        self.stop_disconnected(execution, StopPolicy::ForRemoval)
            .await?;
        let scan = process::scan(HashSet::from([execution.id.clone()])).await?;
        ensure!(
            self.inspect_execution(execution, &scan)
                .await?
                .command_stopped(),
            "process state of the demo remains uncertain; check its processes, then stop it again"
        );
        self.forget_execution(&execution.id).await
    }

    /// Ask the connected wrappers among `executions` to stop, returning the
    /// rest. Native scans run after the connection map is released, as a
    /// workspace stop runs them.
    async fn request_demo_stops(&self, executions: Vec<String>) -> Vec<String> {
        let request = StopRequest {
            reason: "stopped by shoal demo stop".into(),
            save: Save::Nothing,
        };
        let connections = self.executions.connections().await;
        let mut disconnected = Vec::new();
        for id in executions {
            if connections.is_connected(&id) {
                let _ = connections.stop(&id, &request, false).await;
            } else {
                disconnected.push(id);
            }
        }
        disconnected
    }
}

/// Resolve a demo's place before deleting the execution cascades its record.
pub(super) fn execution_launch(db: &Connection, execution: &str) -> Result<Option<Launch>> {
    let record: Option<String> = db
        .query_row(
            "SELECT record FROM demos WHERE execution_id=?1",
            [execution],
            |row| row.get(0),
        )
        .optional()?;
    record
        .map(|record| serde_json::from_str(&record).context("invalid demo record"))
        .transpose()
}

/// Record `launch` as the demo `execution` runs, in the transaction that
/// registers the execution. A record lasts as long as its execution, so a
/// name still recorded belongs to a demo that runs or is stopping.
pub(super) fn record(
    tx: &Transaction<'_>,
    execution: &str,
    workspace: &str,
    launch: &Launch,
) -> Result<()> {
    crate::demos::validate_name(&launch.name)?;
    let taken = crate::daemon::store::exists(
        tx,
        "SELECT 1 FROM demos WHERE workspace_id=?1 AND name=?2",
        params![workspace, launch.name],
    )?;
    ensure!(
        !taken,
        "demo {} is already running; stop it before starting another",
        launch.name
    );
    tx.execute(
        "INSERT INTO demos(execution_id,workspace_id,name,started_at,record) VALUES (?1,?2,?3,?4,?5)",
        params![
            execution,
            workspace,
            launch.name,
            crate::time::unix_seconds() as i64,
            serde_json::to_string(launch)?
        ],
    )?;
    Ok(())
}

fn list(db: &Connection, workspace: Option<&str>) -> Result<Vec<Running>> {
    let mut statement = db.prepare(
        "SELECT d.workspace_id,w.name,d.execution_id,d.started_at,d.record,d.has_run
         FROM demos d JOIN workspaces w ON w.id=d.workspace_id
         WHERE ?1 IS NULL OR d.workspace_id=?1 ORDER BY w.name,d.name",
    )?;
    let rows = statement.query_map([workspace], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, bool>(5)?,
        ))
    })?;
    rows.map(|row| {
        let (workspace_id, workspace, execution_id, started_at, record, has_run) = row?;
        let launch: Launch = serde_json::from_str(&record).context("invalid demo record")?;
        Ok(Running {
            workspace_id,
            workspace,
            execution_id,
            started_at: started_at as u64,
            has_run,
            url: launch.url(),
            launch,
        })
    })
    .collect()
}

/// Note in the transaction registering an execution's child that its demo,
/// if it runs one, has started its command.
pub(super) fn mark_run(tx: &Transaction<'_>, execution: &str) -> Result<()> {
    tx.execute(
        "UPDATE demos SET has_run=1 WHERE execution_id=?1",
        [execution],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
