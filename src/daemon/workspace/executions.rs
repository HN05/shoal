//! Daemon-side execution registration and stopping. Terminal I/O stays in
//! crate::execution, in the invoking CLI process.
use super::{GuardMode, Manager, ResourceGuard};
use crate::{
    daemon::{scope::Caller, store},
    hooks::HookKind,
    model::{Execution, ExecutionPlan, LandPlan, Workspace},
    process::{
        execution::Processes,
        identity::{self as process, Identity},
    },
    protocol::timing,
    state::{ExecutionState, WorkspaceState, states},
};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use std::{collections::HashSet, path::PathBuf, time::Duration};
use tokio::{
    sync::{OwnedMutexGuard, watch},
    time::{Instant, sleep},
};
use uuid::Uuid;

states!(
    /// What a tracked execution runs, which decides its lifecycle rules.
    ExecutionKind {
        /// A caller-chosen command in a ready workspace.
        Command => "command",
        /// A landing authorized by an unscoped caller; the daemon holds its Git gate.
        Land => "land",
        /// The repository's setup command; exclusive apart from the execution that
        /// requested it, and its exit decides whether the workspace becomes ready.
        Setup => "setup",
    }
);

/// The connection must retain the Git guard through execution completion.
#[derive(Debug)]
pub(crate) struct StartedExecution {
    pub plan: ExecutionPlan,
    pub stop: watch::Receiver<bool>,
    pub _git_guard: Option<OwnedMutexGuard<()>>,
}

#[derive(Default)]
struct PreparedExecution {
    setup_cmd: Option<PathBuf>,
    pre_setup: Option<PathBuf>,
    registration_guard: Option<OwnedMutexGuard<()>>,
    lifetime_guard: Option<OwnedMutexGuard<()>>,
    land: Option<Box<LandPlan>>,
    _resources: Option<ResourceGuard>,
}

impl ExecutionKind {
    pub fn start_timeout(self) -> Duration {
        match self {
            Self::Command => timing::EXECUTION_START_TIMEOUT,
            Self::Land | Self::Setup => timing::PREPARED_EXECUTION_START_TIMEOUT,
        }
    }

    /// Apply the kind's lifecycle checks in the registration transaction.
    fn reserve(
        self,
        tx: &Transaction<'_>,
        workspace_id: &str,
        parent_execution: Option<&str>,
    ) -> Result<()> {
        match self {
            Self::Command | Self::Land => store::require_ready(tx, workspace_id)?,
            Self::Setup => {
                let blocking: Option<String> = tx.query_row(
                    "SELECT id FROM executions WHERE workspace_id=?1 AND (?2 IS NULL OR id != ?2) LIMIT 1",
                    params![workspace_id, parent_execution],
                    |r| r.get(0),
                ).optional()?;
                if let Some(blocking) = blocking {
                    bail!(
                        "workspace has active or unknown execution {blocking}; setup is unavailable until it finishes or is cleared"
                    );
                }
                let reserved = tx.execute(
                    "UPDATE workspaces SET state=?2,error=NULL,setup_finished=0 WHERE id=?1 AND state IN (?3,?4,?2)",
                    params![
                        workspace_id,
                        WorkspaceState::Preparing,
                        WorkspaceState::Ready,
                        WorkspaceState::Failed
                    ],
                )?;
                ensure!(reserved == 1, "workspace is busy");
            }
        }
        Ok(())
    }
}

impl Manager {
    /// Register an execution and issue its scope token. The returned receiver
    /// fires when the workspace asks its commands to stop.
    pub(crate) async fn begin_execution(
        &self,
        selector: &str,
        wrapper: Option<Identity>,
        kind: ExecutionKind,
        parent_execution: Option<&str>,
    ) -> Result<StartedExecution> {
        let parent_execution = parent_execution.map(str::to_owned);
        if let Some(wrapper) = &wrapper {
            ensure!(
                process::alive(wrapper)?,
                "execution wrapper is no longer alive"
            );
        }
        let workspace = self.workspace(selector).await?;
        let preparation = self.prepare_execution(&workspace, kind).await?;
        let PreparedExecution {
            setup_cmd,
            pre_setup,
            registration_guard,
            lifetime_guard,
            land,
            _resources,
        } = preparation;
        // Coordinate registration and stop notification without holding the map
        // during any external command or lifetime of the agent.
        let mut connections = self.connections.lock().await;
        let workspace = self.workspace(&workspace.id).await?;
        ensure!(workspace.path.is_dir(), "workspace directory is missing");
        self.verify_worktree(&workspace).await?;
        self.touch(&workspace.id).await;
        let id = Uuid::new_v4().to_string();
        let (execution_id, workspace_id) = (id.clone(), workspace.id.clone());
        let ports = self
            .store
            .run(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                kind.reserve(&tx, &workspace_id, parent_execution.as_deref())?;
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
                setup: kind == ExecutionKind::Setup,
                workspace_id: workspace.id.clone(),
            },
        )
        .await;
        drop(connections);
        drop(registration_guard);
        if let Some(command) = pre_setup
            && let Err(error) = async {
                crate::hooks::run_detached(
                    crate::hooks::Hook::PreSetup,
                    &workspace,
                    &command,
                    &self.paths,
                )
                .await?;
                self.verify_worktree(&workspace).await
            }
            .await
        {
            self.finish_execution(id, kind, Some(1)).await?;
            self.set_state(
                &workspace.id,
                WorkspaceState::Failed,
                Some(format!("{error:#}")),
            )
            .await?;
            return Err(error);
        }
        Ok(StartedExecution {
            plan: ExecutionPlan {
                id,
                workspace,
                scope_token,
                setup_cmd,
                ports,
                land,
            },
            stop: receiver,
            _git_guard: lifetime_guard,
        })
    }

    async fn prepare_execution(
        &self,
        workspace: &Workspace,
        kind: ExecutionKind,
    ) -> Result<PreparedExecution> {
        match kind {
            ExecutionKind::Command => Ok(PreparedExecution::default()),
            ExecutionKind::Land => {
                let guard = self
                    .git_gate(&workspace.repository_id)
                    .await
                    .lock_owned()
                    .await;
                let land = self.prepare_land(&workspace.id).await?;
                Ok(PreparedExecution {
                    land: Some(Box::new(land)),
                    lifetime_guard: Some(guard),
                    ..Default::default()
                })
            }
            ExecutionKind::Setup => {
                let git_guard = self
                    .git_gate(&workspace.repository_id)
                    .await
                    .lock_owned()
                    .await;
                let setup_cmd = self.workspace_hook(workspace, HookKind::Setup).await?;
                let pre_setup = self.workspace_hook(workspace, HookKind::PreSetup).await?;
                ensure!(
                    setup_cmd.is_some() || pre_setup.is_some(),
                    "no setup_cmd configured"
                );
                let mode = if pre_setup.is_some() {
                    GuardMode::Exclusive
                } else {
                    GuardMode::Shared
                };
                let resources = self.resource_guard(&workspace.id, mode).await?;
                Ok(PreparedExecution {
                    setup_cmd,
                    pre_setup,
                    registration_guard: Some(git_guard),
                    _resources: Some(resources),
                    ..Default::default()
                })
            }
        }
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
                            "setup failed (exit {code}, processes stopped: {complete}); retry with shoal setup"
                        )
                    });
                    let state = if error.is_none() {
                        WorkspaceState::Ready
                    } else {
                        WorkspaceState::Failed
                    };
                    tx.execute(
                        "UPDATE workspaces SET state=?2,error=?3,setup_finished=?4 WHERE id=(SELECT workspace_id FROM executions WHERE id=?1) AND state=?5",
                        params![
                            record_id,
                            state,
                            error,
                            error.is_none(),
                            WorkspaceState::Preparing
                        ],
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
            "owned processes survived stopping; retry after shoal doctor"
        );
        // Manual removal retains its policy: unrelated/unverifiable processes do
        // not block deletion. A plain stop must not claim those processes stopped.
        if !manual_removal {
            ensure!(
                processes.launch_recorded && processes.group_candidates.is_empty(),
                "execution ownership is incomplete; use shoal doctor to inspect it"
            );
            let after = Processes::inspect(execution, &after).await?;
            ensure!(
                !after.has_survivors() && after.visibility_complete(),
                "process state remains uncertain; use shoal doctor to inspect it"
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
        let deadline = Instant::now() + timing::WORKSPACE_STOP_TIMEOUT;
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
            sleep(timing::WORKSPACE_STOP_POLL_INTERVAL).await;
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
