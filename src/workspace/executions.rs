//! Daemon-side execution registration and stopping. Terminal I/O stays in
//! crate::execution, in the invoking CLI process.
use super::Manager;
use crate::{
    execution_processes::Processes,
    model::{Execution, ExecutionPlan},
    process_identity::{self as process, Identity},
    scope::Caller,
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

/// What a tracked execution runs, which decides its lifecycle rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionKind {
    /// A caller-chosen command in a ready workspace.
    Command,
    /// A landing authorized by an unscoped caller; the daemon holds its Git gate.
    Land,
    /// The repository's setup command; exclusive, and its exit decides whether
    /// the workspace becomes ready.
    Setup,
}

impl Manager {
    /// Register an execution and issue its scope token. The returned receiver
    /// fires when the workspace asks its commands to stop.
    pub async fn begin_execution(
        &self,
        selector: &str,
        wrapper: Option<Identity>,
        kind: ExecutionKind,
    ) -> Result<(ExecutionPlan, watch::Receiver<bool>)> {
        if let Some(wrapper) = &wrapper {
            ensure!(
                process::alive(wrapper)?,
                "execution wrapper is no longer alive"
            );
        }
        let setup = kind == ExecutionKind::Setup;
        // Coordinate registration and stop notification without holding the map
        // during any external command or lifetime of the agent.
        let workspace = self.workspace(selector).await?;
        let gate = self.git_gate(&workspace.repository_id).await;
        let _guard = if setup { Some(gate.lock().await) } else { None };
        let mut connections = self.connections.lock().await;
        let workspace = self.workspace(&workspace.id).await?;
        let setup_cmd = if setup {
            let command = self
                .workspace_config(&workspace)
                .await?
                .setup_cmd
                .context("no setup_cmd configured")?;
            Some(workspace.path.join(command))
        } else {
            None
        };
        ensure!(workspace.path.is_dir(), "workspace directory is missing");
        self.verify_worktree(&workspace).await?;
        self.touch(&workspace.id).await;
        let id = Uuid::new_v4().to_string();
        let (execution_id, workspace_id) = (id.clone(), workspace.id.clone());
        let ports = self
            .store
            .run(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                if setup {
                    let busy: bool = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM executions WHERE workspace_id=?1)",
                        [&workspace_id],
                        |r| r.get(0),
                    )?;
                    ensure!(!busy, "workspace has active or unknown executions");
                    let reserved = tx.execute(
                        "UPDATE workspaces SET state=?2,error=NULL WHERE id=?1 AND state IN (?3,?4,?2)",
                        params![
                            workspace_id,
                            WorkspaceState::Preparing,
                            WorkspaceState::Ready,
                            WorkspaceState::Failed
                        ],
                    )?;
                    ensure!(reserved == 1, "workspace is busy");
                } else {
                    store::require_ready(&tx, &workspace_id)?;
                }
                tx.execute(
                    "INSERT INTO executions(id,workspace_id,state,wrapper) VALUES (?1,?2,?3,?4)",
                    params![
                        execution_id,
                        workspace_id,
                        ExecutionState::Running,
                        store::json_text(wrapper.as_ref())?
                    ],
                )?;
                let ports = store::ports(&tx, Some(&workspace_id))?;
                tx.commit()?;
                Ok(ports)
            })
            .await?;
        let (sender, receiver) = watch::channel(false);
        connections.insert(id.clone(), sender);
        let scope_token = Uuid::new_v4().to_string();
        self.issue_scope(
            scope_token.clone(),
            Caller {
                execution_id: id.clone(),
                landing: kind == ExecutionKind::Land,
                workspace_id: workspace.id.clone(),
            },
        )
        .await;
        Ok((
            ExecutionPlan {
                id,
                workspace,
                scope_token,
                setup_cmd,
                ports,
                land: None,
            },
            receiver,
        ))
    }

    pub async fn record_execution_child(
        &self,
        id: String,
        child: Option<Identity>,
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
        self.store
            .run(move |db| {
                let updated = db.execute(
                    "UPDATE executions SET child=?2,group_id=?3 WHERE id=?1 AND group_id IS NULL",
                    params![id, store::json_text(child.as_ref())?, group_id],
                )?;
                ensure!(
                    updated == 1,
                    "execution already registered or no longer exists"
                );
                Ok(())
            })
            .await
    }

    /// Close an execution. `exit_code` is `None` when the wrapper disconnected
    /// without reporting; such executions stay recorded as unknown. Returns
    /// whether every owned process is verifiably gone.
    pub async fn finish_execution(
        &self,
        id: String,
        kind: ExecutionKind,
        exit_code: Option<i32>,
    ) -> Result<bool> {
        let complete = match exit_code {
            Some(_) => match process::scan(HashSet::from([id.clone()])).await {
                Ok(scan) => scan.processes.is_empty(),
                Err(_) => false,
            },
            None => false,
        };
        let mut connections = self.connections.lock().await;
        let record_id = id.clone();
        self.store
            .run(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                if kind == ExecutionKind::Setup {
                    let code = exit_code.unwrap_or(1);
                    let error = (!(complete && code == 0)).then(|| {
                        format!(
                            "setup failed (exit {code}, processes stopped: {complete}); retry with shoal prepare"
                        )
                    });
                    let state = if error.is_none() {
                        WorkspaceState::Ready
                    } else {
                        WorkspaceState::Failed
                    };
                    tx.execute(
                        "UPDATE workspaces SET state=?2,error=?3 WHERE id=(SELECT workspace_id FROM executions WHERE id=?1) AND state=?4",
                        params![record_id, state, error, WorkspaceState::Preparing],
                    )?;
                }
                if complete {
                    tx.execute("DELETE FROM executions WHERE id=?1", [record_id])?;
                } else {
                    tx.execute(
                        "UPDATE executions SET state=?2 WHERE id=?1",
                        params![record_id, ExecutionState::Unknown],
                    )?;
                }
                tx.commit()?;
                Ok(())
            })
            .await?;
        connections.remove(&id);
        self.scopes
            .lock()
            .await
            .retain(|_, caller| caller.execution_id != id);
        Ok(complete)
    }

    pub async fn stop_workspace(&self, selector: &str) -> Result<()> {
        let workspace = self.workspace(selector).await?;
        self.reserve_lifecycle(&workspace.id, WorkspaceState::Stopping)
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

    async fn stop_disconnected(&self, execution: &Execution, manual_removal: bool) -> Result<()> {
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

    /// Ask connected wrappers to stop and signal disconnected survivors.
    /// `allow_disconnected` relaxes the ownership proof for manual removal.
    pub(super) async fn stop_executions(&self, id: &str, allow_disconnected: bool) -> Result<()> {
        let id = id.to_owned();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let connections = self.connections.lock().await;
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
                match connections
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
            drop(connections);
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
        self.connections
            .lock()
            .await
            .get(id)
            .is_some_and(|sender| !sender.is_closed())
    }
}
