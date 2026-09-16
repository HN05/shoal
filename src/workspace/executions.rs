//! Daemon-side execution registration and stopping. Terminal I/O stays in
//! crate::execution, in the invoking CLI process.
use super::Manager;
use crate::{
    execution_processes::Processes,
    model::ExecutionPlan,
    process_identity as process,
    state::{ExecutionState, WorkspaceState},
    store,
};
use anyhow::{Context, Result, ensure};
use rusqlite::{TransactionBehavior, params};
use std::{collections::HashSet, time::Duration};
use tokio::{
    sync::watch,
    time::{Instant, sleep},
};
use uuid::Uuid;

impl Manager {
    pub async fn begin(
        &self,
        selector: String,
        wrapper: Option<process::Identity>,
    ) -> Result<(ExecutionPlan, watch::Receiver<bool>)> {
        if let Some(wrapper) = &wrapper {
            ensure!(
                process::alive(wrapper)?,
                "execution wrapper is no longer alive"
            );
        }
        // Coordinate registration and stop notification without holding the map
        // during any external command or lifetime of the agent.
        let mut active = self.active.lock().await;
        let workspace = self.get(selector).await?;
        ensure!(workspace.path.is_dir(), "workspace directory is missing");
        self.verify_worktree(&workspace).await?;
        *self
            .activity
            .lock()
            .await
            .entry(workspace.id.clone())
            .or_default() += 1;
        let id = Uuid::new_v4().to_string();
        let (execution_id, workspace_id) = (id.clone(), workspace.id.clone());
        let ports = self
            .store
            .run(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let ready: bool = tx.query_row(
                    "SELECT state='ready' FROM workspaces WHERE id=?1",
                    [&workspace_id],
                    |r| r.get(0),
                )?;
                ensure!(ready, "workspace is not ready");
                tx.execute(
                    "INSERT INTO executions(id,workspace_id,state,wrapper) VALUES (?1,?2,?3,?4)",
                    params![
                        execution_id,
                        workspace_id,
                        ExecutionState::Running,
                        wrapper.map(|w| serde_json::to_string(&w)).transpose()?
                    ],
                )?;
                let ports = store::ports(&tx, Some(&workspace_id))?;
                tx.commit()?;
                Ok(ports)
            })
            .await?;
        let (sender, receiver) = watch::channel(false);
        active.insert(id.clone(), sender);
        let scope_token = Uuid::new_v4().to_string();
        self.scopes
            .lock()
            .await
            .insert(scope_token.clone(), (id.clone(), workspace.id.clone()));
        Ok((
            ExecutionPlan {
                scope_token,
                id,
                workspace,
                ports,
            },
            receiver,
        ))
    }

    pub async fn record_execution_child(
        &self,
        id: String,
        child: Option<process::Identity>,
        group_id: u32,
    ) -> Result<()> {
        ensure!(
            group_id > 1 && group_id <= i32::MAX as u32,
            "invalid execution process group"
        );
        ensure!(
            child.as_ref().is_none_or(|child| child.pid == group_id),
            "child must lead its execution process group"
        );
        self.store.run(move |db| {
            ensure!(db.execute("UPDATE executions SET child=?2,group_id=?3 WHERE id=?1 AND group_id IS NULL",
                params![id, child.map(|c| serde_json::to_string(&c)).transpose()?, group_id])? == 1,
                "execution already registered or no longer exists");
            Ok(())
        }).await
    }

    pub async fn finish(&self, id: String, complete: bool) -> Result<bool> {
        let complete = if complete {
            match process::scan(HashSet::from([id.clone()])).await {
                Ok(scan) => scan.processes.is_empty(),
                Err(_) => false,
            }
        } else {
            false
        };
        let mut active = self.active.lock().await;
        let record_id = id.clone();
        self.store
            .run(move |db| {
                if complete {
                    db.execute("DELETE FROM executions WHERE id=?1", [record_id])?;
                } else {
                    db.execute(
                        "UPDATE executions SET state=?2 WHERE id=?1",
                        params![record_id, ExecutionState::Unknown],
                    )?;
                }
                Ok(())
            })
            .await?;
        active.remove(&id);
        self.scopes
            .lock()
            .await
            .retain(|_, (execution, _)| execution != &id);
        Ok(complete)
    }

    pub async fn stop(&self, selector: String) -> Result<()> {
        let workspace = self.get(selector).await?;
        let id = workspace.id.clone();
        self.store
            .run(move |db| {
                ensure!(
                    db.execute(
                        "UPDATE workspaces SET state=?2 WHERE id=?1 AND state IN (?3,?4)",
                        params![
                            id,
                            WorkspaceState::Stopping,
                            WorkspaceState::Ready,
                            WorkspaceState::Failed
                        ]
                    )? == 1,
                    "workspace is busy or not ready"
                );
                Ok(())
            })
            .await?;
        let result = self.stop_executions(&workspace.id, false).await;
        self.set_state(
            &workspace.id,
            workspace.state,
            result
                .as_ref()
                .err()
                .map(|e| format!("{e:#}"))
                .or(workspace.error),
        )
        .await?;
        result
    }

    async fn stop_disconnected(
        &self,
        execution: &crate::model::Execution,
        manual_removal: bool,
    ) -> Result<()> {
        let scan = process::scan(HashSet::from([execution.id.clone()])).await?;
        let processes = Processes::inspect(execution, &scan).await?;
        processes.stop().await?;
        let after = process::scan(HashSet::from([execution.id.clone()])).await?;
        ensure!(
            after.processes.is_empty(),
            "owned processes survived stopping; retry after shoal reconcile"
        );
        // Manual removal retains its policy: unrelated/unverifiable processes do
        // not block deletion. A plain stop must not claim those processes stopped.
        if !manual_removal {
            ensure!(
                processes.launch_recorded && processes.group_candidates.is_empty(),
                "execution ownership is incomplete; use shoal reconcile to inspect it"
            );
            let after = Processes::inspect(execution, &after).await?;
            ensure!(
                !after.has_survivors() && after.visibility_complete(),
                "process state remains uncertain; use shoal reconcile to inspect it"
            );
            let id = execution.id.clone();
            self.store
                .run(move |db| {
                    db.execute("DELETE FROM executions WHERE id=?1", [id])?;
                    Ok(())
                })
                .await?;
        }
        Ok(())
    }

    pub(super) async fn stop_executions(&self, id: &str, allow_disconnected: bool) -> Result<()> {
        let id = id.to_owned();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let active = self.active.lock().await;
            let query_id = id.clone();
            let executions = self
                .store
                .run(move |db| store::executions(db, &query_id))
                .await?;
            if executions.is_empty() {
                return Ok(());
            }
            let mut connected = 0;
            let mut disconnected = Vec::new();
            for execution in executions {
                match active
                    .get(&execution.id)
                    .filter(|sender| !sender.is_closed())
                {
                    Some(sender) => {
                        sender
                            .send(true)
                            .context("execution disconnected during stop")?;
                        connected += 1;
                    }
                    None => disconnected.push(execution),
                }
            }
            // Registration and notifications are coordinated under the map lock.
            // Native scans and TERM/KILL waits must not block other workspaces'
            // launches or completion acknowledgements. This workspace's lifecycle
            // reservation already prevents new executions here.
            drop(active);
            for execution in disconnected {
                self.stop_disconnected(&execution, allow_disconnected)
                    .await?;
            }
            if connected == 0 {
                return Ok(());
            }
            ensure!(
                Instant::now() < deadline,
                "timed out waiting for workspace processes to stop"
            );
            sleep(Duration::from_millis(50)).await;
        }
    }
    pub(crate) async fn execution_connected(&self, id: &str) -> bool {
        self.active
            .lock()
            .await
            .get(id)
            .is_some_and(|sender| !sender.is_closed())
    }
}
