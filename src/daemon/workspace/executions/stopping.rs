//! Stopping executions: the stop policy and requests, and the operations
//! that stop a workspace's executions, connected or disconnected.
use super::super::{Manager, stopped_work::Stopper};
use crate::client::execution_ids::ExecutionId;
use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::{
        space_processes::{Selection, WorktreeTies},
        store,
    },
    fsutil::off_runtime,
    model::{Execution, QueueEntry, StoppedWorkspace, Workspace},
    process::identity::{self as process, Identity},
    protocol::{QueuedLaunches, timing},
    state::{ExecutionState, WorkspaceState},
};
use anyhow::{Result, ensure};
use std::collections::HashSet;
use tokio::time::{Instant, sleep};

use crate::daemon::conversions::ConversionStop;

#[cfg(test)]
mod tests;

/// The workspaces with a running execution. One whose wrapper is gone is
/// left for `shoal stop <workspace>` rather than failing every bulk stop.
fn running_workspaces(db: &rusqlite::Connection) -> Result<HashSet<SpaceId>> {
    Ok(db
        .prepare("SELECT DISTINCT workspace_id FROM executions WHERE state=?1")?
        .query_map([ExecutionState::Running], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// The workspaces in which a chat's agent has yet to restart after a
/// conversion, which runs where the chat was whatever the workspace's state.
fn converting_workspaces(db: &rusqlite::Connection) -> Result<HashSet<SpaceId>> {
    Ok(db
        .prepare(
            "SELECT space_id FROM space_conversions
                WHERE json_extract(record,'$.to')='workspace'",
        )?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// The proof required after stopping a disconnected execution. Both policies
/// signal only native identity-verified processes and reject owned survivors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::daemon::workspace) enum StopPolicy {
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
    pub execution: ExecutionId,
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
    /// the system or the daemon shuts down, with restores that need no user
    /// queued.
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

    /// The stop before the system or the daemon shuts down, for `reason`.
    pub(crate) fn shutdown(reason: &str) -> Self {
        Self {
            reason: reason.into(),
            save: Save::Shutdown,
        }
    }

    /// Removal of a missing worktree, which keeps no checkpoint or record.
    pub(in crate::daemon::workspace) fn removal() -> Self {
        Self {
            reason: "the workspace is being removed".into(),
            save: Save::Nothing,
        }
    }

    /// Removal of an existing worktree, whose removal record keeps its agents.
    pub(in crate::daemon::workspace) fn restorable_removal() -> Self {
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

/// What a stop keeps of the agents it stopped.
#[derive(Clone, Copy)]
enum Keep<'a> {
    /// Their recovery records, as the stop saved them.
    Records,
    /// Last sessions, which stay down until `shoal resume`, recording the
    /// workspace whose process stopped them when given.
    LastSessions {
        stopped_by: Option<&'a SpaceId>,
        launches: QueuedLaunches,
    },
}

impl Manager {
    /// The settled workspaces `shoal stop --all` stops: those with a running
    /// execution or a stopped agent whose restore is requested or queued,
    /// and those a waiting launch opens when the stop cancels `launches`.
    /// The queue gate keeps restores and launches from moving between the
    /// queue and their agents meanwhile, and executions are read before
    /// requests, which an agent a signal stops saves before its execution
    /// ends.
    pub async fn stop_targets(&self, launches: QueuedLaunches) -> Result<Vec<Workspace>> {
        let _gate = self.agent_queue.lock_admission().await;
        let mut targets = self.store.run(|db| running_workspaces(db)).await?;
        let converting = self.store.run(|db| converting_workspaces(db)).await?;
        targets.extend(converting.iter().cloned());
        targets.extend(self.pending_restore_workspaces().await?);
        if launches == QueuedLaunches::Cancel {
            targets.extend(self.waiting_launch_workspaces().await?);
        }
        Ok(self
            .list_workspaces()
            .await?
            .into_iter()
            .filter(|workspace| {
                let stoppable = matches!(
                    workspace.state,
                    WorkspaceState::Ready | WorkspaceState::Failed
                );
                (stoppable || converting.contains(&workspace.id)) && targets.contains(&workspace.id)
            })
            .collect())
    }

    /// Stop every execution, cancelling agents' waiting restores.
    pub async fn stop_workspace(
        &self,
        selector: &SpaceSelector,
        request: StopRequest,
    ) -> Result<()> {
        self.stop_workspace_executions(
            selector,
            request,
            Protected::Stop,
            StopPolicy::RequireCompleteProof,
            Keep::Records,
        )
        .await?;
        Ok(())
    }

    /// Stop as [`Manager::stop_workspace`] does at the user's request, then
    /// keep the stopped agents as last sessions, which stay down until the
    /// user resumes them, or the process of workspace `stopped_by` that
    /// stopped them. With `acknowledge_unproven`, executions whose launch
    /// identity was never recorded count as ended once nothing of them is
    /// visible, because the user checked. Returns the waiting launches it
    /// cancelled as `launches` says. A CI job's workspace cancels the job's
    /// run on the forge instead, whose runner then stops it.
    pub async fn stop_workspace_manually(
        &self,
        selector: &SpaceSelector,
        acknowledge_unproven: bool,
        stopped_by: Option<&SpaceId>,
        launches: QueuedLaunches,
    ) -> Result<StoppedWorkspace> {
        if let Some(run) = self.cancel_job_in(selector).await? {
            return Ok(StoppedWorkspace {
                cancelled: Vec::new(),
                cancelled_run: Some(run),
            });
        }
        let policy = if acknowledge_unproven {
            StopPolicy::Acknowledged
        } else {
            StopPolicy::RequireCompleteProof
        };
        let keep = Keep::LastSessions {
            stopped_by,
            launches,
        };
        let cancelled = self
            .stop_workspace_executions(
                selector,
                StopRequest::manual(),
                Protected::Stop,
                policy,
                keep,
            )
            .await?;
        Ok(StoppedWorkspace {
            cancelled,
            cancelled_run: None,
        })
    }

    /// Stop executions other than agents that protection already stopped,
    /// saving records as `shoal stop` does.
    pub async fn stop_unprotected(&self, selector: &SpaceSelector, reason: String) -> Result<()> {
        let request = StopRequest {
            reason,
            save: Save::Everything,
        };
        self.stop_keeping_restores(selector, request).await
    }

    /// Stop executions other than agents that protection already stopped
    /// for `request`, keeping their restores and those the stopped agents
    /// request.
    pub(in crate::daemon::workspace) async fn stop_keeping_restores(
        &self,
        selector: &SpaceSelector,
        request: StopRequest,
    ) -> Result<()> {
        self.stop_workspace_executions(
            selector,
            request,
            Protected::Keep,
            StopPolicy::RequireCompleteProof,
            Keep::Records,
        )
        .await?;
        Ok(())
    }

    /// Stop the executions of the workspace `selector` names and `keep` what
    /// they saved, all under one reservation: no execution registers
    /// meanwhile, and no other stop keeps the saved agents first. Returns
    /// the waiting launches `keep` cancels.
    async fn stop_workspace_executions(
        &self,
        selector: &SpaceSelector,
        request: StopRequest,
        protected: Protected,
        policy: StopPolicy,
        keep: Keep<'_>,
    ) -> Result<Vec<QueueEntry>> {
        let workspace = self.workspace(selector).await?;
        // A stop that keeps the agents down until shoal resume keeps the
        // chat a conversion makes of the workspace down too.
        let stop = match keep {
            Keep::LastSessions { stopped_by, .. } => ConversionStop {
                stopped_by,
                holds: true,
            },
            Keep::Records => ConversionStop {
                stopped_by: None,
                holds: false,
            },
        };
        // Setup is no agent's, so a workspace still preparing after a
        // conversion has only the converting agent to stop.
        if self.stop_converting_agent(&workspace.id, stop).await?
            && workspace.state == WorkspaceState::Preparing
        {
            return Ok(Vec::new());
        }
        let reserved_state = self
            .reserve_lifecycle(&workspace.id, WorkspaceState::Stopping)
            .await?;
        let result = async {
            let stopping = self.unstopped_executions(&workspace.id).await?;
            // Before stopping, so an admission cannot start one after.
            let cancelled = match keep {
                Keep::LastSessions {
                    launches: QueuedLaunches::Cancel,
                    ..
                } => self.cancel_waiting_launches(&workspace.id).await?,
                Keep::LastSessions { .. } | Keep::Records => Vec::new(),
            };
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
            if let Keep::LastSessions { stopped_by, .. } = keep {
                let stopper = stopped_by.map(|workspace| Stopper {
                    workspace,
                    executions: stopping,
                });
                self.keep_stopped_as_last_sessions(
                    &SpaceSelector::id(&workspace.id),
                    stopper.as_ref(),
                )
                .await?;
            }
            anyhow::Ok(cancelled)
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

    pub(in crate::daemon::workspace) async fn stop_disconnected(
        &self,
        execution: &Execution,
        policy: StopPolicy,
    ) -> Result<()> {
        let scan = process::scan(HashSet::from([execution.id.clone()])).await?;
        let processes = self.inspect_execution(execution, &scan).await?;
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
        let after = self.inspect_execution(execution, &after).await?;
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
    async fn ensure_directory_unused(&self, workspace_id: &SpaceId) -> Result<()> {
        let path = self.workspace_by_id(workspace_id).await?.path;
        let users = if off_runtime::is_dir(&path).await? {
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

    /// Stop every untracked process of the workspace being removed, such as
    /// servers its hooks or apps started, or scripts a recording left
    /// behind, since none outlives removal. Its worktree ties processes to
    /// it as `ties` says once the directory is verified to be its worktree
    /// still.
    pub(in crate::daemon::workspace) async fn stop_removed_processes(
        &self,
        workspace: &Workspace,
        ties: WorktreeTies,
    ) -> Result<()> {
        if ties != WorktreeTies::None {
            self.verify_worktree(workspace).await?;
        }
        let space = self.workspace_space(workspace, ties).await?;
        self.stop_untracked(space, Selection::Processes, None)
            .await?;
        Ok(())
    }

    /// Ask connected wrappers to stop and signal disconnected survivors.
    pub(in crate::daemon::workspace) async fn stop_executions(
        &self,
        id: &SpaceId,
        policy: StopPolicy,
        request: &StopRequest,
    ) -> Result<()> {
        self.stop_executions_keeping(id, policy, request, Protected::Stop)
            .await
    }

    /// Wrappers decide what to save; setup and merge save nothing.
    async fn stop_executions_keeping(
        &self,
        id: &SpaceId,
        policy: StopPolicy,
        request: &StopRequest,
        protected: Protected,
    ) -> Result<()> {
        self.hand_over_before_removal(id).await?;
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
}
