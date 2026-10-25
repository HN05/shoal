//! Demos the daemon records with the executions that run them, so a demo
//! ends with its execution: when it stops, its workspace is removed or its
//! wrapper finishes.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::{
    Manager,
    executions::{Save, StopRequest},
};
use crate::{
    daemon::log,
    demos::{Launch, Preset, Running},
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

    /// Ask the workspace's demo `name`, or every demo there, to stop. Their
    /// records go once their wrappers finish.
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
        let connections = self.connections.lock().await;
        let mut requests = self.stop_requests.lock().await;
        for id in executions {
            let Some(sender) = connections.get(&id).filter(|sender| !sender.is_closed()) else {
                continue;
            };
            requests.insert(
                id.clone(),
                StopRequest {
                    reason: "stopped by shoal demo stop".into(),
                    save: Save::Nothing,
                },
            );
            let _ = sender.send(true);
        }
        Ok(())
    }

    /// The demo `execution` runs, if it runs one.
    pub(crate) async fn demo_of(&self, execution: &str) -> Result<Option<Launch>> {
        let execution = execution.to_owned();
        self.store
            .run(move |db| {
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
            })
            .await
    }

    /// After a demo's execution ended, give up the simulator lease simulator
    /// demos took once none of the workspace's runs any more. The daemon does
    /// it, since the worker's scope may have ended with its agent.
    pub(crate) async fn demo_ended(&self, workspace: &str, launch: &Launch) {
        if launch.preset != Some(Preset::Simulator) || !launch.leased_simulator {
            return;
        }
        let released = async {
            let still_used = self
                .demos(Some(workspace))
                .await?
                .iter()
                .any(|demo| demo.launch.preset == Some(Preset::Simulator));
            if !still_used {
                self.release_simulator(workspace, crate::demos::SIM_LEASE.into())
                    .await?;
            }
            anyhow::Ok(())
        };
        if let Err(error) = released.await {
            log!("simulator of demo {} not released: {error:#}", launch.name);
        }
    }
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
        "SELECT d.workspace_id,w.name,d.execution_id,d.started_at,d.record
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
        ))
    })?;
    rows.map(|row| {
        let (workspace_id, workspace, execution_id, started_at, record) = row?;
        Ok(Running {
            workspace_id,
            workspace,
            execution_id,
            started_at: started_at as u64,
            launch: serde_json::from_str(&record).context("invalid demo record")?,
        })
    })
    .collect()
}

#[cfg(test)]
mod tests;
