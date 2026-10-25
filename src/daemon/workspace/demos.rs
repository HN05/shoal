//! Demos the daemon records with the executions that run them, so a demo
//! ends with its execution: when it stops, its workspace is removed or its
//! wrapper finishes.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, Transaction, params};

use super::{
    Manager,
    executions::{Save, StopPolicy, StopRequest},
};
use crate::demos::{Launch, Running};

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
                self.stop_disconnected(&execution, StopPolicy::RequireCompleteProof)
                    .await?;
            }
        }
        Ok(())
    }

    /// Ask the connected wrappers among `executions` to stop, returning the
    /// rest. Native scans run after the connection map is released, as a
    /// workspace stop runs them.
    async fn request_demo_stops(&self, executions: Vec<String>) -> Vec<String> {
        let connections = self.connections.lock().await;
        let mut requests = self.stop_requests.lock().await;
        let mut disconnected = Vec::new();
        for id in executions {
            let Some(sender) = connections.get(&id).filter(|sender| !sender.is_closed()) else {
                disconnected.push(id);
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
        disconnected
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
