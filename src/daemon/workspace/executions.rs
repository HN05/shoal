//! Daemon-side execution registration and stopping. Terminal I/O stays in
//! crate::execution, in the invoking CLI process.
mod completion;

use super::{GuardMode, Manager, ResourceGuard, places::ExecutionPlace};
use crate::daemon::log;
use crate::{
    config::repo::Cache,
    daemon::{scope::Caller, store},
    demos::Launch,
    hooks::HookKind,
    model::{Execution, ExecutionPlan, LandPlan, PortReservation, Repository, Workspace},
    process::{
        execution::Processes,
        identity::{self as process, Identity},
    },
    protocol::{Reattach, timing},
    state::{ExecutionState, WorkspaceState, states},
};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    sync::{OwnedMutexGuard, watch},
    time::{Instant, sleep},
};
use uuid::Uuid;

mod state;
/// The workspaces with a running execution. One whose wrapper is gone is
/// left for `shoal stop <workspace>` rather than failing every bulk stop.
fn running_workspaces(db: &rusqlite::Connection) -> Result<HashSet<String>> {
    Ok(db
        .prepare("SELECT DISTINCT workspace_id FROM executions WHERE state=?1")?
        .query_map([ExecutionState::Running], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests;

pub(crate) use state::{Completing, Executions};

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

/// The proof required after stopping a disconnected execution. Both policies
/// signal only native identity-verified processes and reject owned survivors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StopPolicy {
    /// Retain uncertain records; only clear executions proven fully stopped.
    RequireCompleteProof,
    /// As `RequireCompleteProof`, but the user confirmed that executions
    /// without a recorded launch identity ended once nothing visible survives.
    Acknowledged,
    /// Stop verified processes, leaving records for successful workspace removal.
    /// Unrelated or unverifiable processes do not block that removal.
    ForRemoval,
}

/// A disconnected execution with nothing of it visible whose launch identity
/// Shoal never recorded, so it cannot prove the execution ended. The user may
/// check and confirm that it did.
#[derive(Debug)]
pub struct UnprovenStop {
    pub execution: String,
}

impl std::fmt::Display for UnprovenStop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "execution {} has no visible processes, but Shoal never recorded how it was launched, so it cannot prove the execution ended",
            self.execution
        )
    }
}

impl std::error::Error for UnprovenStop {}

/// The process IDs of `processes`, for a message.
fn pids<'a>(processes: impl IntoIterator<Item = &'a Identity>) -> String {
    processes
        .into_iter()
        .map(|process| process.pid.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Why the daemon stops executions, which their wrappers show, and what the
/// stop saves for `shoal resume`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopRequest {
    pub reason: String,
    pub save: Save,
}

/// What a stop saves for `shoal resume`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Save {
    Nothing,
    /// Agent sessions and interrupted commands.
    Everything,
    /// Agent sessions, which the removed workspace's removal record keeps.
    RemovedAgents,
    /// Agent sessions and interrupted commands, saved before the stop since
    /// the system shuts down, with restores that need no user queued.
    Shutdown,
}

impl StopRequest {
    /// The stop `shoal stop` requests.
    pub fn manual() -> Self {
        Self {
            reason: "stopped by shoal stop".into(),
            save: Save::Everything,
        }
    }

    /// The stop `shoal queue cancel` requests of an agent waiting for its
    /// restore.
    pub fn queue_cancel() -> Self {
        Self {
            reason: "its restore was cancelled with shoal queue cancel".into(),
            save: Save::Everything,
        }
    }

    pub fn repair() -> Self {
        Self {
            reason: "stopped to repair the workspace".into(),
            save: Save::Nothing,
        }
    }

    /// The stop before the system shuts down.
    pub(crate) fn system_shutdown() -> Self {
        Self {
            reason: "the system is shutting down".into(),
            save: Save::Shutdown,
        }
    }

    /// Removal of a missing worktree, which keeps no checkpoint or record.
    pub(super) fn removal() -> Self {
        Self {
            reason: "the workspace is being removed".into(),
            save: Save::Nothing,
        }
    }

    /// Removal of an existing worktree, whose removal record keeps its agents.
    pub(super) fn restorable_removal() -> Self {
        Self {
            save: Save::RemovedAgents,
            ..Self::removal()
        }
    }

    /// Whether the stop is a removal whose removal record keeps the agents.
    pub(crate) fn keeps_removed_agents(&self) -> bool {
        self.save == Save::RemovedAgents
    }

    pub(crate) fn before_shutdown(&self) -> bool {
        self.save == Save::Shutdown
    }

    /// Whether the wrapper saves its agent's session, or its interrupted
    /// command when it runs no agent.
    pub(crate) fn saves(&self, agent: bool) -> bool {
        match self.save {
            Save::Nothing => false,
            Save::Everything | Save::Shutdown => true,
            Save::RemovedAgents => agent,
        }
    }
}

/// Whether a workspace stop ends agents that protection already stopped,
/// cancelling their automatic restore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Protected {
    Stop,
    Keep,
}

/// A restored connection for a wrapper whose command kept running.
#[derive(Debug)]
pub(crate) struct ReattachedExecution {
    pub workspace: Workspace,
    pub stop: watch::Receiver<bool>,
}

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
    copy_ignored: bool,
    pre_setup: Option<PathBuf>,
    registration_guard: Option<OwnedMutexGuard<()>>,
    lifetime_guard: Option<OwnedMutexGuard<()>>,
    land: Option<Box<LandPlan>>,
    _resources: Option<ResourceGuard>,
}

/// The record a new execution persists at registration.
struct ExecutionRecord {
    id: String,
    workspace_id: String,
    kind: ExecutionKind,
    parent_execution: Option<String>,
    wrapper: Option<Identity>,
    scope_token: String,
    demo: Option<Launch>,
    place: Option<ExecutionPlace>,
}

struct RegisteredExecution {
    id: String,
    workspace: Workspace,
    scope_token: String,
    ports: Vec<PortReservation>,
    environment: std::collections::BTreeMap<String, std::ffi::OsString>,
    stop: watch::Receiver<bool>,
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
        // Quarantine may have left a moving workspace failed, which setup
        // would otherwise accept.
        store::ensure_no_pending_move(tx, workspace_id)?;
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
    #[cfg(test)]
    pub(crate) async fn begin_execution(
        &self,
        selector: &str,
        wrapper: Option<Identity>,
        kind: ExecutionKind,
        caller: Option<&Caller>,
    ) -> Result<StartedExecution> {
        self.begin_execution_running(selector, wrapper, kind, caller, None)
            .await
    }

    /// Register an execution and issue its scope token, recording the demo it
    /// runs with it. The returned receiver fires when the workspace asks its
    /// commands to stop.
    #[cfg(test)]
    pub(crate) async fn begin_execution_running(
        &self,
        selector: &str,
        wrapper: Option<Identity>,
        kind: ExecutionKind,
        caller: Option<&Caller>,
        demo: Option<Launch>,
    ) -> Result<StartedExecution> {
        self.begin_execution_placed(selector, wrapper, kind, caller, demo, None)
            .await
    }

    /// Register an execution and issue its scope token, recording the demo it
    /// runs and the `place` it runs at with it. The returned receiver fires
    /// when the workspace asks its commands to stop.
    pub(crate) async fn begin_execution_placed(
        &self,
        selector: &str,
        wrapper: Option<Identity>,
        kind: ExecutionKind,
        caller: Option<&Caller>,
        demo: Option<Launch>,
        place: Option<ExecutionPlace>,
    ) -> Result<StartedExecution> {
        let (workspace, preparation) = self
            .prepare_execution(selector, wrapper.as_ref(), kind, caller.is_some())
            .await?;
        let PreparedExecution {
            setup_cmd,
            copy_ignored,
            pre_setup,
            registration_guard,
            lifetime_guard,
            land,
            _resources,
        } = preparation;
        let registered = self
            .register_execution(
                &workspace.id,
                wrapper,
                kind,
                caller.and_then(Caller::execution_id),
                demo,
                place,
            )
            .await?;
        // Setup releases its Git gate before the hook; resources remain guarded
        // until pre-setup completes, while landing's gate travels with the plan.
        drop(registration_guard);
        self.run_pre_setup(&registered, kind, pre_setup.as_deref())
            .await?;
        Ok(StartedExecution {
            plan: ExecutionPlan {
                id: registered.id,
                workspace: registered.workspace,
                scope_token: registered.scope_token,
                setup_cmd,
                copy_ignored,
                ports: registered.ports,
                environment: registered.environment,
                land,
            },
            stop: registered.stop,
            _git_guard: lifetime_guard,
        })
    }

    async fn register_execution(
        &self,
        workspace_id: &str,
        wrapper: Option<Identity>,
        kind: ExecutionKind,
        parent_execution: Option<&str>,
        demo: Option<Launch>,
        place: Option<ExecutionPlace>,
    ) -> Result<RegisteredExecution> {
        let mut connections = self.executions.connections().await;
        connections.ensure_accepting()?;
        let workspace = self.workspace(workspace_id).await?;
        ensure!(workspace.path.is_dir(), "workspace directory is missing");
        self.verify_worktree(&workspace).await?;
        self.touch(&workspace.id).await;
        let settings = self.workspace_settings(&workspace).await?;
        let repository = self.repository(&workspace.repository_id).await?;
        let id = Uuid::new_v4().to_string();
        let scope_token = Uuid::new_v4().to_string();
        let (ports, caches) = self
            .persist_execution(
                ExecutionRecord {
                    id: id.clone(),
                    workspace_id: workspace.id.clone(),
                    kind,
                    parent_execution: parent_execution.map(str::to_owned),
                    wrapper,
                    scope_token: scope_token.clone(),
                    demo,
                    place,
                },
                repository.clone(),
                settings.caches.clone(),
            )
            .await?;
        let environment = crate::env::render_configured_environment(
            &settings.env,
            &workspace,
            crate::repository::name(&repository),
            &ports,
            &caches,
        );
        let receiver = connections
            .connect(&id, kind, &workspace.id, scope_token.clone(), false)
            .await;
        Ok(RegisteredExecution {
            id,
            workspace,
            scope_token,
            ports,
            environment,
            stop: receiver,
        })
    }

    /// Reserve the execution's lifecycle, insert its running record and
    /// prepare the repository's caches in one transaction, returning the
    /// workspace's ports and the cache directories.
    async fn persist_execution(
        &self,
        record: ExecutionRecord,
        repository: Repository,
        caches: BTreeMap<String, Cache>,
    ) -> Result<(Vec<PortReservation>, BTreeMap<String, PathBuf>)> {
        let ExecutionRecord {
            id,
            workspace_id,
            kind,
            parent_execution,
            wrapper,
            scope_token,
            demo,
            place,
        } = record;
        // A place's ownership moves only under the lock its closes hold.
        let _ownership = match place {
            Some(_) => Some(self.place_ownership.lock().await),
            None => None,
        };
        // Only commands may reattach: setup and landing hold daemon gates
        // that a restart releases.
        let reattach_token = (kind == ExecutionKind::Command).then_some(scope_token);
        self.store
            .run(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                kind.reserve(&tx, &workspace_id, parent_execution.as_deref())?;
                tx.execute(
                    "INSERT INTO executions(id,workspace_id,state,wrapper,scope_token) VALUES (?1,?2,?3,?4,?5)",
                    params![
                        id,
                        workspace_id,
                        ExecutionState::Running,
                        store::json_text(wrapper.as_ref())?,
                        reattach_token
                    ],
                )?;
                if let Some(demo) = &demo {
                    super::demos::record(&tx, &id, &workspace_id, demo)?;
                }
                if let Some(place) = &place {
                    super::places::record_execution_place(&tx, &id, &workspace_id, place)?;
                }
                let ports = store::ports(&tx, Some(&workspace_id))?;
                // Under the write lock emptying caches takes, and undone with
                // the execution's row when the directories cannot be prepared.
                let caches = crate::caches::directories(&repository, &caches)?;
                tx.commit()?;
                Ok((ports, caches))
            })
            .await
    }

    async fn run_pre_setup(
        &self,
        registered: &RegisteredExecution,
        kind: ExecutionKind,
        pre_setup: Option<&Path>,
    ) -> Result<()> {
        let workspace = &registered.workspace;
        if let Some(command) = pre_setup
            && let Err(error) = async {
                crate::hooks::run_detached(
                    crate::hooks::Hook::PreSetup,
                    workspace,
                    command,
                    &self.paths,
                    &registered.environment,
                )
                .await?;
                self.verify_worktree(workspace).await
            }
            .await
        {
            self.finish_execution(registered.id.clone(), kind, Some(1))
                .await?;
            self.set_state(
                &workspace.id,
                WorkspaceState::Failed,
                Some(format!("{error:#}")),
            )
            .await?;
            return Err(error);
        }
        Ok(())
    }

    async fn prepare_execution(
        &self,
        selector: &str,
        wrapper: Option<&Identity>,
        kind: ExecutionKind,
        scoped: bool,
    ) -> Result<(Workspace, PreparedExecution)> {
        if let Some(wrapper) = wrapper {
            ensure!(
                process::alive(wrapper)?,
                "execution wrapper is no longer alive"
            );
        }
        let workspace = self.workspace(selector).await?;
        let preparation = self
            .prepare_execution_kind(&workspace, kind, scoped)
            .await?;
        Ok((workspace, preparation))
    }

    async fn prepare_execution_kind(
        &self,
        workspace: &Workspace,
        kind: ExecutionKind,
        scoped: bool,
    ) -> Result<PreparedExecution> {
        match kind {
            ExecutionKind::Command => Ok(PreparedExecution::default()),
            ExecutionKind::Land => {
                let guard = self.gates.lock_git(&workspace.repository_id).await;
                let land = self.prepare_land(&workspace.id).await?;
                if scoped {
                    self.authorize_scoped_land(&land).await?;
                }
                Ok(PreparedExecution {
                    land: Some(Box::new(land)),
                    lifetime_guard: Some(guard),
                    ..Default::default()
                })
            }
            ExecutionKind::Setup => {
                let git_guard = self.gates.lock_git(&workspace.repository_id).await;
                self.ensure_no_pending_rename(&workspace.id).await?;
                let setup_cmd = self.workspace_hook(workspace, HookKind::Setup).await?;
                let pre_setup = self.workspace_hook(workspace, HookKind::PreSetup).await?;
                let copy_ignored = self.workspace_settings(workspace).await?.copy_ignored;
                ensure!(
                    setup_cmd.is_some() || pre_setup.is_some() || copy_ignored,
                    "no setup configured; set setup_cmd, pre_setup_cmd or copy_ignored"
                );
                let mode = if pre_setup.is_some() {
                    GuardMode::Exclusive
                } else {
                    GuardMode::Shared
                };
                let resources = self.gates.resource_guard(&workspace.id, mode)?;
                Ok(PreparedExecution {
                    setup_cmd,
                    copy_ignored,
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
                let tx = db.transaction()?;
                let updated = tx.execute(
                    "UPDATE executions SET child=?2,group_id=?3 WHERE id=?1 AND group_id IS NULL",
                    params![id, store::json_text(child.as_ref())?, group_id],
                )?;
                ensure!(
                    updated == 1,
                    "execution already registered or no longer exists"
                );
                super::demos::mark_run(&tx, &id)?;
                // A restored agent takes over the slot its restore kept.
                tx.execute("DELETE FROM agent_queue WHERE execution_id=?1", [&id])?;
                tx.commit()?;
                Ok(())
            })
            .await
    }

    /// Forget the exited child of an execution whose wrapper starts another,
    /// once what it left running is proven stopped, as a paused agent's is,
    /// and the worktree the next run starts in is still the recorded one.
    /// Unproven survivors keep the child's identity, so the next run cannot
    /// register over the evidence.
    pub(crate) async fn release_exited_child(&self, id: &str, workspace: &Workspace) -> Result<()> {
        match self
            .pause_agent_execution(id, &workspace.id, timing::PROCESS_PROOF_TIMEOUT)
            .await?
        {
            super::agents::Pause::Paused => {}
            super::agents::Pause::Unproven(issue) => {
                bail!("the previous run left processes Shoal cannot prove stopped: {issue}")
            }
        }
        self.verify_worktree(workspace).await
    }

    /// Close an execution. `exit_code` is `None` when the wrapper disconnected
    /// without reporting; such executions stay recorded as unknown. Returns
    /// whether the command's processes are verifiably gone. The reporting
    /// wrapper remains alive until its completion acknowledgement.
    pub async fn finish_execution(
        &self,
        id: String,
        kind: ExecutionKind,
        exit_code: Option<i32>,
    ) -> Result<bool> {
        self.forget_proxy_retry(&id, exit_code.is_some()).await;
        let scan = match exit_code {
            Some(_) => {
                // Daemon-side reservations can be registered without a
                // wrapper while a command is prepared. There is no process
                // ownership to reconcile in that state, and a whole-process
                // scan on macOS can observe an unrelated process whose
                // environment is disappearing. Skip that inventory until a
                // wrapper, child, or process group has been recorded.
                let query_id = id.clone();
                let needs_scan = self
                    .store
                    .run(move |db| {
                        Ok(
                            store::find_execution(db, &query_id)?.is_none_or(|execution| {
                                execution.wrapper.is_some()
                                    || execution.child.is_some()
                                    || execution.group_id.is_some()
                            }),
                        )
                    })
                    .await?;
                Some(if needs_scan {
                    process::scan(HashSet::from([id.clone()])).await
                } else {
                    Ok(process::Scan::default())
                })
            }
            None => None,
        };
        self.finish_execution_after_scan(id, kind, exit_code, scan)
            .await
    }

    pub(super) async fn finish_execution_after_scan(
        &self,
        id: String,
        kind: ExecutionKind,
        exit_code: Option<i32>,
        scan: Option<Result<process::Scan>>,
    ) -> Result<bool> {
        let issue = self.execution_completion_issue(&id, scan).await?;
        let complete = issue.is_none();
        self.keep_stopped_turn(&id, None).await;
        let connections = self.executions.connections().await;
        let demo = self
            .record_execution_completion(&id, kind, exit_code, issue)
            .await?;
        if connections.disconnect(&id).await {
            self.agent_queue.wake();
        }
        if !complete {
            self.signals.execution_became_unknown();
        }
        self.close_demo_session(demo).await;
        Ok(complete)
    }

    /// Restore the connection of a wrapper that kept its command running while
    /// the daemon was away. The record, the scope token issued at launch and
    /// the wrapper and child identities must all match.
    pub(crate) async fn reattach_execution(
        &self,
        request: &Reattach,
    ) -> Result<ReattachedExecution> {
        ensure!(
            process::alive(&request.wrapper)?,
            "execution wrapper is no longer alive"
        );
        let mut connections = self.executions.connections().await;
        ensure!(
            !connections.is_connected(&request.execution),
            "execution is already connected"
        );
        let workspace_id = self.authorize_reattachment(request).await?;
        let workspace = self.workspace(&workspace_id).await?;
        // Before protection can know the agent again and save a new stop.
        // A wrapper awaiting a restore has received its stop.
        if request.awaiting_restore.is_none() {
            self.discard_undelivered_stop(&workspace, &request.execution);
        }
        // A stopped agent stays stopped until its restore.
        let stop = connections
            .connect(
                &request.execution,
                ExecutionKind::Command,
                &workspace.id,
                request.scope_token.clone(),
                request.awaiting_restore.is_some(),
            )
            .await;
        drop(connections);
        // A restore whose agent runs again ended, even if the previous daemon
        // stopped before removing it.
        if request.awaiting_restore.is_none() {
            self.remove_restore(&request.execution).await;
        }
        if let Some(agent) = &request.agent {
            self.track_agent(
                &request.execution,
                agent,
                &workspace.id,
                &workspace.name,
                request.recover,
                Duration::from_millis(request.running_ms),
            )
            .await;
            if let Some(protection) = &request.awaiting_restore {
                self.await_agent_restore(&request.execution, protection.clone())
                    .await;
            }
        }
        self.touch(&workspace.id).await;
        Ok(ReattachedExecution { workspace, stop })
    }

    /// Check a reattaching wrapper against its execution record and the scope
    /// token issued at launch, then mark the execution running, returning its
    /// workspace ID. The caller holds the connections lock until it connects
    /// the wrapper, so no other wrapper connects in between.
    async fn authorize_reattachment(&self, request: &Reattach) -> Result<String> {
        let request = request.clone();
        self.store
            .run(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let execution = store::find_execution(&tx, &request.execution)?
                    .context("execution record is missing")?;
                let token: Option<String> = tx.query_row(
                    "SELECT scope_token FROM executions WHERE id=?1",
                    [&request.execution],
                    |row| row.get(0),
                )?;
                ensure!(
                    token.as_deref() == Some(request.scope_token.as_str()),
                    "execution scope token does not match"
                );
                ensure!(
                    reattaching_identity_matches(&execution, &request),
                    "execution process identity does not match its record"
                );
                tx.execute(
                    "UPDATE executions SET state=?2 WHERE id=?1",
                    params![request.execution, ExecutionState::Running],
                )?;
                tx.commit()?;
                Ok(execution.workspace_id)
            })
            .await
    }

    /// A reattaching wrapper still runs its command, so a stop the previous
    /// daemon saved but never delivered no longer describes the agent.
    fn discard_undelivered_stop(&self, workspace: &Workspace, execution: &str) {
        match crate::execution::recovery::discard_undelivered(&self.paths, &workspace.id, execution)
        {
            Ok(true) => log!(
                "discarded an undelivered stop for execution {execution} in {}",
                workspace.name
            ),
            Ok(false) => {}
            Err(error) => {
                log!("undelivered stop for execution {execution} not discarded: {error:#}")
            }
        }
    }

    /// Drop a wrapper's connection without recording completion, so its
    /// execution awaits reattachment.
    pub(crate) async fn detach_execution(&self, id: &str) {
        if self.executions.disconnect(id).await {
            self.agent_queue.wake();
        }
    }

    async fn execution_completion_issue(
        &self,
        id: &str,
        scan: Option<Result<process::Scan>>,
    ) -> Result<Option<String>> {
        let scan = match scan {
            Some(Ok(scan)) => scan,
            Some(Err(error)) => return Ok(Some(format!("process inventory failed: {error:#}"))),
            None => {
                return Ok(Some(
                    "wrapper disconnected without reporting its exit".into(),
                ));
            }
        };
        let query_id = id.to_owned();
        let execution = self
            .store
            .run(move |db| store::find_execution(db, &query_id))
            .await?;
        let Some(execution) = execution else {
            return Ok(Some("execution record is missing".into()));
        };
        Ok(match Processes::inspect(&execution, &scan).await {
            Ok(processes) => processes.completion_issue(),
            Err(error) => Some(format!("process inspection failed: {error:#}")),
        })
    }

    /// The settled workspaces `shoal stop --all` stops: those with a running
    /// execution or a stopped agent whose restore is requested or queued. The
    /// queue gate keeps restores from moving between the queue and their
    /// agents meanwhile, and executions are read before requests, which an
    /// agent a signal stops saves before its execution ends.
    pub async fn stop_targets(&self) -> Result<Vec<Workspace>> {
        let _gate = self.agent_queue.lock_admission().await;
        let mut targets = self.store.run(|db| running_workspaces(db)).await?;
        targets.extend(self.pending_restore_workspaces().await?);
        Ok(self
            .list_workspaces()
            .await?
            .into_iter()
            .filter(|workspace| {
                matches!(
                    workspace.state,
                    WorkspaceState::Ready | WorkspaceState::Failed
                ) && targets.contains(&workspace.id)
            })
            .collect())
    }

    /// Stop every execution, cancelling agents' waiting restores.
    pub async fn stop_workspace(&self, selector: &str, request: StopRequest) -> Result<()> {
        self.stop_workspace_executions(
            selector,
            request,
            Protected::Stop,
            StopPolicy::RequireCompleteProof,
        )
        .await
    }

    /// Stop as [`Manager::stop_workspace`] does at the user's request, then
    /// keep the stopped agents as last sessions, which stay down until the
    /// user resumes them. With `acknowledge_unproven`, executions whose launch
    /// identity was never recorded count as ended once nothing of them is
    /// visible, because the user checked.
    pub async fn stop_workspace_manually(
        &self,
        selector: &str,
        acknowledge_unproven: bool,
    ) -> Result<()> {
        let policy = if acknowledge_unproven {
            StopPolicy::Acknowledged
        } else {
            StopPolicy::RequireCompleteProof
        };
        self.stop_workspace_executions(selector, StopRequest::manual(), Protected::Stop, policy)
            .await?;
        self.keep_stopped_as_last_sessions(selector).await
    }

    /// Stop executions other than agents that protection already stopped,
    /// saving records as `shoal stop` does.
    pub async fn stop_unprotected(&self, selector: &str, reason: String) -> Result<()> {
        let request = StopRequest {
            reason,
            save: Save::Everything,
        };
        self.stop_keeping_restores(selector, request).await
    }

    /// Stop executions other than agents that protection already stopped
    /// for `request`, keeping their restores and those the stopped agents
    /// request.
    pub(super) async fn stop_keeping_restores(
        &self,
        selector: &str,
        request: StopRequest,
    ) -> Result<()> {
        self.stop_workspace_executions(
            selector,
            request,
            Protected::Keep,
            StopPolicy::RequireCompleteProof,
        )
        .await
    }

    async fn stop_workspace_executions(
        &self,
        selector: &str,
        request: StopRequest,
        protected: Protected,
        policy: StopPolicy,
    ) -> Result<()> {
        let workspace = self.workspace(selector).await?;
        let reserved_state = self
            .reserve_lifecycle(&workspace.id, WorkspaceState::Stopping)
            .await?;
        let result = async {
            // Before stopping, so a restore admitted meanwhile cannot start
            // after, and again once stopped, for the request of an agent a
            // signal was ending meanwhile.
            if protected == Protected::Stop {
                self.cancel_stopped_restores(&workspace.id).await?;
            }
            self.stop_executions_keeping(&workspace.id, policy, &request, protected)
                .await?;
            if protected == Protected::Stop {
                self.cancel_stopped_restores(&workspace.id).await?;
            }
            anyhow::Ok(())
        }
        .await;
        // A stop waiting for the user's confirmation has not failed.
        let failure = result
            .as_ref()
            .err()
            .filter(|error| error.downcast_ref::<UnprovenStop>().is_none());
        self.set_state(
            &workspace.id,
            reserved_state,
            failure.map(|e| format!("{e:#}")).or(workspace.error),
        )
        .await?;
        result
    }

    /// Before the daemon exits, ask every connected execution to stop. Wrappers
    /// that can reattach detach instead and keep their commands running or
    /// their wait for an agent's restore; the rest stop as `shoal stop` does, so a restart leaves resumable records
    /// instead of disconnected executions. Requests already accepted cannot
    /// register afterwards, and wrappers that miss the bound are left for the
    /// startup audit.
    pub async fn stop_for_shutdown(&self) {
        let deadline = Instant::now() + timing::WORKSPACE_STOP_TIMEOUT;
        self.executions
            .shut_down(StopRequest {
                reason: "the Shoal daemon shut down".into(),
                save: Save::Everything,
            })
            .await;
        while Instant::now() < deadline && self.executions.has_connections().await {
            sleep(timing::WORKSPACE_STOP_POLL_INTERVAL).await;
        }
    }

    pub(super) async fn stop_disconnected(
        &self,
        execution: &Execution,
        policy: StopPolicy,
    ) -> Result<()> {
        let scan = process::scan(HashSet::from([execution.id.clone()])).await?;
        let processes = Processes::inspect(execution, &scan).await?;
        processes.stop().await?;
        let after = process::scan(HashSet::from([execution.id.clone()])).await?;
        ensure!(
            after.processes.is_empty(),
            "processes of execution {} survived stopping: {}",
            execution.id,
            pids(after.processes.iter().map(|process| &process.identity))
        );
        if policy == StopPolicy::ForRemoval {
            return Ok(());
        }
        ensure!(
            processes.group_candidates.is_empty(),
            "processes Shoal cannot verify share the process group of execution {}: {}; stop them yourself, then run shoal stop again",
            execution.id,
            pids(&processes.group_candidates)
        );
        let after = Processes::inspect(execution, &after).await?;
        ensure!(
            !after.has_survivors(),
            "execution {} still has processes after stopping; check them, then run shoal stop again",
            execution.id
        );
        ensure!(
            after.launch_recorded || policy == StopPolicy::Acknowledged,
            UnprovenStop {
                execution: execution.id.clone(),
            }
        );
        ensure!(
            after.unreadable.is_empty(),
            "Shoal cannot read the environment of processes that may belong to execution {}: {}; check them, then run shoal stop again",
            execution.id,
            pids(&after.unreadable)
        );
        if !after.launch_recorded {
            self.ensure_directory_unused(&execution.workspace_id)
                .await?;
        }
        self.forget_execution(&execution.id).await
    }

    /// Fail while processes use the workspace's directory: an acknowledgement
    /// that an unprovable execution ended cannot cover a visible process.
    async fn ensure_directory_unused(&self, workspace_id: &str) -> Result<()> {
        let path = self.workspace(workspace_id).await?.path;
        let users = if path.is_dir() {
            crate::process::in_directory(&path).await?
        } else {
            Vec::new()
        };
        ensure!(
            users.is_empty(),
            "processes still use the workspace directory: {}; stop them, then run shoal stop again",
            users.join(", ")
        );
        Ok(())
    }

    /// Stop every verified process that still carries the workspace's identity,
    /// such as servers its hooks or apps started, since none outlives removal.
    pub(super) async fn stop_workspace_processes(&self, id: &str) -> Result<()> {
        let survivors = process::scan_workspace(id).await?;
        if survivors.is_empty() {
            return Ok(());
        }
        process::stop_verified(&survivors).await?;
        let remaining = process::scan_workspace(id).await?;
        ensure!(
            remaining.is_empty(),
            "workspace processes survived stopping: {}; stop them yourself, then retry",
            pids(&remaining)
        );
        Ok(())
    }

    /// Ask connected wrappers to stop and signal disconnected survivors.
    pub(super) async fn stop_executions(
        &self,
        id: &str,
        policy: StopPolicy,
        request: &StopRequest,
    ) -> Result<()> {
        self.stop_executions_keeping(id, policy, request, Protected::Stop)
            .await
    }

    /// Wrappers decide what to save; setup and landing save nothing.
    async fn stop_executions_keeping(
        &self,
        id: &str,
        policy: StopPolicy,
        request: &StopRequest,
        protected: Protected,
    ) -> Result<()> {
        let id = id.to_owned();
        let deadline = Instant::now() + timing::WORKSPACE_STOP_TIMEOUT;
        loop {
            let connections = self.executions.connections().await;
            let query_id = id.clone();
            let executions = self
                .store
                .run(move |db| store::executions(db, &query_id))
                .await?;
            if executions.is_empty() {
                return Ok(());
            }
            let mut waiting = Vec::new();
            let mut disconnected = Vec::new();
            for execution in executions {
                if !connections.is_connected(&execution.id) {
                    disconnected.push(execution);
                } else if connections
                    .stop(&execution.id, request, protected == Protected::Keep)
                    .await?
                {
                    waiting.push(execution);
                }
            }
            // Registration and notifications are coordinated under the map lock.
            // Native scans and TERM/KILL waits must not block other workspaces'
            // launches or completion acknowledgements. This workspace's lifecycle
            // reservation already prevents new executions here.
            drop(connections);
            for execution in disconnected {
                self.stop_disconnected(&execution, policy).await?;
            }
            if waiting.is_empty() {
                return Ok(());
            }
            ensure!(
                Instant::now() < deadline,
                "timed out waiting for workspace processes to stop: {}",
                waiting
                    .iter()
                    .map(|execution| self.stop_progress(execution))
                    .collect::<Vec<_>>()
                    .join("; ")
            );
            sleep(timing::WORKSPACE_STOP_POLL_INTERVAL).await;
        }
    }

    /// How far a connected execution got towards stopping, so a stop that
    /// timed out names the step it waited on.
    fn stop_progress(&self, execution: &Execution) -> String {
        let step = match self.executions.completing(&execution.id) {
            Some(step) => step.to_string(),
            None if execution.child.is_none() => "command not started".to_owned(),
            None => "command still running".to_owned(),
        };
        format!("execution {}: {step}", execution.id)
    }

    /// Delete an execution record whose processes the caller proved stopped.
    pub(crate) async fn forget_execution(&self, id: &str) -> Result<()> {
        self.keep_stopped_turn(id, None).await;
        let demo = self.delete_execution_record(id).await?;
        self.close_demo_session(demo).await;
        Ok(())
    }
}

/// Whether a reattaching wrapper is the one the record names. One awaiting a
/// restore also matches once the daemon has forgotten its stopped child.
fn reattaching_identity_matches(execution: &Execution, request: &Reattach) -> bool {
    let child_matches =
        execution.child == request.child && execution.group_id == Some(request.group_id);
    // A wrapper waiting to restore its agent, or between a demo's runs,
    // names a child the daemon may have forgotten once it proved it stopped.
    let child_forgotten = execution.child.is_none()
        && execution.group_id.is_none()
        && (request.awaiting_restore.is_some() || request.between_runs);
    execution.wrapper.as_ref() == Some(&request.wrapper) && (child_matches || child_forgotten)
}
