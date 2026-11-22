//! Stopping executions: the stop policy and requests, and the operations
//! that stop a workspace's executions, connected or disconnected.
use super::Connections;
use crate::Agents;
use crate::fsutil::off_runtime;
use crate::log;
use crate::model::Execution;
use crate::model::PreparationWait;
use crate::model::QueueEntry;
use crate::model::Space;
use crate::model::StoppedSpace;
use crate::model::Workspace;
use crate::process::execution::Processes;
use crate::process::identity as process;
use crate::process::identity::Identity;
use crate::protocol::QueuedLaunches;
use crate::protocol::timing;
use crate::state::ExecutionState;
use crate::state::WorkspaceState;
use anyhow::{Result, ensure};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::model::ByKind;
use shoal_protocol::space_names::SpaceSelector;
use shoal_store;
use std::collections::HashSet;
use tokio::time::{Instant, sleep};

use crate::daemon::Daemon;
use shoal_daemon_base::jobs::JobStop;

/// The workspaces with a running execution. One whose wrapper is gone is
/// left for `shoal stop <workspace>` rather than failing every bulk stop.
fn running_workspaces(db: &rusqlite::Connection) -> Result<HashSet<SpaceId>> {
    Ok(db
        .prepare("SELECT DISTINCT space_id FROM executions WHERE state=?1")?
        .query_map([ExecutionState::Running], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Whether `space` has settled, so a bulk stop may act on it: a worktree
/// that is ready or whose setup failed; a space without a worktree has no
/// setup to wait for.
fn settled(space: &Space) -> bool {
    (space.worktree()).is_none_or(|worktree| {
        matches!(
            worktree.state,
            WorkspaceState::Ready | WorkspaceState::Failed
        )
    })
}

/// Whether an agent waits unstopped for the preparation of `space`'s
/// worktree.
fn awaits_preparation(space: &Space) -> bool {
    (space.worktree())
        .and_then(|worktree| worktree.preparation_waits.as_ref())
        .is_some_and(|waits| *waits != PreparationWait::Stopped)
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
pub enum StopPolicy {
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

    /// The stop of an agent protection stops for `reason` that the store
    /// could not record, which saves its session for `shoal resume`.
    pub fn unrecorded(reason: &str) -> Self {
        Self {
            reason: format!("{reason}; its stop could not be recorded"),
            save: Save::Everything,
        }
    }

    /// The stop of a space's agents before `shoal move --ssh` exports it,
    /// which saves their sessions for the import on the other machine, or
    /// for their restores here when that import fails.
    pub fn moving() -> Self {
        Self {
            reason: "its space moves to another machine".into(),
            save: Save::Everything,
        }
    }

    /// The stop of a space's agents before its removal here, since another
    /// machine took the space over while this one was offline, which saves
    /// their sessions in its removal record.
    pub fn taken_over() -> Self {
        Self {
            reason: "another machine took its space over".into(),
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
    pub fn shutdown(reason: &str) -> Self {
        Self {
            reason: reason.into(),
            save: Save::Shutdown,
        }
    }

    /// The agent of a chat that became a workspace, whose session its
    /// conversion keeps to continue in the worktree.
    pub fn conversion() -> Self {
        Self {
            reason: "its chat became a workspace".into(),
            save: Save::Nothing,
        }
    }

    /// Removal of a missing worktree, which keeps no checkpoint or record.
    pub fn removal() -> Self {
        Self {
            reason: "the workspace is being removed".into(),
            save: Save::Nothing,
        }
    }

    /// Removal of a chat or an existing worktree, whose removal record keeps
    /// its agents.
    pub fn restorable_removal() -> Self {
        Self {
            save: Save::RemovedAgents,
            ..Self::removal()
        }
    }

    /// Whether the stop is a removal whose removal record keeps the agents.
    pub fn keeps_removed_agents(&self) -> bool {
        self.save == Save::RemovedAgents
    }

    pub fn before_shutdown(&self) -> bool {
        self.save == Save::Shutdown
    }

    /// Whether the wrapper saves its agent's session, or its interrupted
    /// command when it runs no agent.
    pub fn saves(&self, agent: bool) -> bool {
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

/// How a stop ends a space's executions and what it keeps of them.
#[derive(Clone, Copy)]
struct Stopping<'a> {
    request: &'a StopRequest,
    protected: Protected,
    policy: StopPolicy,
    keep: Keep,
}

/// What a stop keeps of the agents it stopped.
#[derive(Clone, Copy)]
enum Keep {
    /// Their recovery records, as the stop saved them.
    Records,
    /// Last sessions, which stay down until `shoal resume`, cancelling
    /// the waiting launches as `launches` says.
    LastSessions { launches: QueuedLaunches },
}

impl Agents {
    /// The spaces `shoal stop --all` stops: the settled spaces with a
    /// running execution or a stopped agent whose restore is requested or
    /// queued, and the workspaces a waiting launch opens when the stop
    /// cancels `launches`, and then too those whose preparation waits
    /// unstopped, with an agent or none.
    /// The queue gate keeps restores and launches from moving between the
    /// queue and their agents meanwhile, and executions are read before
    /// requests, which an agent a signal stops saves before its execution
    /// ends.
    pub async fn stop_targets(
        &self,
        daemon: &dyn Daemon,
        launches: QueuedLaunches,
    ) -> Result<Vec<Space>> {
        let _gate = self.agent_queue.lock_admission().await;
        let mut targets = self.store.run(|db| running_workspaces(db)).await?;
        let converting = self.store.run(|db| converting_workspaces(db)).await?;
        targets.extend(converting.iter().cloned());
        targets.extend(self.pending_restore_workspaces().await?);
        if launches == QueuedLaunches::Cancel {
            targets.extend(self.waiting_launch_workspaces().await?);
        }
        let cancels = launches == QueuedLaunches::Cancel;
        let spaces = daemon.present_spaces().await?;
        Ok((spaces.into_iter())
            .filter(|space| {
                let settled = settled(space) || converting.contains(&space.id);
                settled && targets.contains(&space.id) || cancels && awaits_preparation(space)
            })
            .collect())
    }

    /// Stop every execution of the space `selector` names, cancelling its
    /// agents' waiting restores.
    pub async fn stop_space(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        request: StopRequest,
    ) -> Result<()> {
        self.stop_space_executions(
            daemon,
            selector,
            request,
            Protected::Stop,
            StopPolicy::RequireCompleteProof,
            Keep::Records,
        )
        .await?;
        Ok(())
    }

    /// Refuse a stop of `workspace` while another lifecycle operation holds
    /// it. One whose preparation waits runs nothing, so its stop only
    /// cancels the agent waiting there.
    pub async fn check_stop(&self, daemon: &dyn Daemon, workspace: &Workspace) -> Result<()> {
        match workspace.preparation_waits {
            Some(_) => Ok(()),
            None => daemon.check_lifecycle(&workspace.id).await,
        }
    }

    /// Stop as [`Manager::stop_space`] does at the user's request, then
    /// keep the stopped agents as last sessions, which stay down until
    /// something resumes them. With `acknowledge_unproven`, executions whose
    /// launch identity was never recorded count as ended once nothing of them
    /// is visible, because the caller confirmed it. Returns the waiting
    /// launches it cancelled as `launches` says. A CI job's workspace cancels the job's
    /// run on the forge instead, whose runner then stops it.
    pub async fn stop_space_manually(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        acknowledge_unproven: bool,
        launches: QueuedLaunches,
    ) -> Result<StoppedSpace> {
        if let Some(run) = (daemon.cancel_job_in(selector, JobStop::Stop)).await? {
            return Ok(StoppedSpace {
                cancelled_run: Some(run),
                ..StoppedSpace::default()
            });
        }
        let policy = if acknowledge_unproven {
            StopPolicy::Acknowledged
        } else {
            StopPolicy::RequireCompleteProof
        };
        let keep = Keep::LastSessions { launches };
        let cancelled = self
            .stop_space_executions(
                daemon,
                selector,
                StopRequest::manual(),
                Protected::Stop,
                policy,
                keep,
            )
            .await?;
        Ok(StoppedSpace {
            cancelled,
            ..StoppedSpace::default()
        })
    }

    /// Stop executions other than agents that protection already stopped,
    /// saving records as `shoal stop` does.
    pub async fn stop_unprotected(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        reason: String,
    ) -> Result<()> {
        let request = StopRequest {
            reason,
            save: Save::Everything,
        };
        self.stop_keeping_restores(daemon, selector, request).await
    }

    /// Stop executions other than agents that protection already stopped
    /// for `request`, keeping their restores and those the stopped agents
    /// request.
    pub async fn stop_keeping_restores(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        request: StopRequest,
    ) -> Result<()> {
        self.stop_space_executions(
            daemon,
            selector,
            request,
            Protected::Keep,
            StopPolicy::RequireCompleteProof,
            Keep::Records,
        )
        .await?;
        Ok(())
    }

    /// Stop the executions of the space `selector` names and `keep` what
    /// they saved: a workspace's under one reservation of its lifecycle, so
    /// no execution registers meanwhile and no other stop keeps the saved
    /// agents first, while a chat runs only its agent. Returns the waiting
    /// launches `keep` cancels.
    async fn stop_space_executions(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        request: StopRequest,
        protected: Protected,
        policy: StopPolicy,
        keep: Keep,
    ) -> Result<Vec<QueueEntry>> {
        let stopping = Stopping {
            request: &request,
            protected,
            policy,
            keep,
        };
        match daemon.space(selector).await?.by_kind() {
            ByKind::Workspace(workspace) => {
                self.stop_workspace_executions(daemon, workspace, stopping)
                    .await
            }
            ByKind::Chat(chat) => self.stop_and_keep(daemon, &chat.id, stopping).await,
        }
    }

    /// [`Self::stop_space_executions`] of `workspace`, under the reservation
    /// of its lifecycle.
    async fn stop_workspace_executions(
        &self,
        daemon: &dyn Daemon,
        workspace: Workspace,
        stopping: Stopping<'_>,
    ) -> Result<Vec<QueueEntry>> {
        // A stop that keeps the agents down until shoal resume keeps the
        // chat a conversion makes of the workspace down too.
        let holds = matches!(stopping.keep, Keep::LastSessions { .. });
        // Setup is no agent's, so a workspace still preparing after a
        // conversion has only the converting agent to stop.
        if workspace.state == WorkspaceState::Preparing
            && daemon.stop_converting_agent(&workspace.id, holds).await?
        {
            return Ok(Vec::new());
        }
        // Nothing runs where the preparation waits, which goes on waiting;
        // one that started meanwhile is stopped as any preparing workspace.
        if workspace.preparation_waits.is_some()
            && let Some(cancelled) =
                (self.stop_waiting(daemon, &workspace.id, &stopping.keep)).await?
        {
            return Ok(cancelled);
        }
        let reserved_state = daemon
            .reserve_lifecycle(&workspace.id, WorkspaceState::Stopping)
            .await?;
        let result = async {
            // Once reserved, so a conversion recorded meanwhile, which needs
            // a ready workspace, is found here.
            daemon.stop_converting_agent(&workspace.id, holds).await?;
            self.stop_and_keep(daemon, &workspace.id, stopping).await
        }
        .await;
        // A stop waiting for the user's confirmation has not failed.
        let failure = result
            .as_ref()
            .err()
            .filter(|error| error.downcast_ref::<UnprovenStop>().is_none());
        daemon
            .set_state(
                &workspace.id,
                reserved_state,
                failure.map(|e| format!("{e:#}")).or(workspace.part.error),
            )
            .await?;
        // The stopped agents' leases waited for the stop to end.
        if let Err(error) = daemon.release_ended_leases_of(&workspace.id).await {
            log!(
                "leases of the agents stopped in {} not released: {error:#}",
                workspace.name
            );
        }
        result
    }

    /// Cancel the launches waiting to open the space `id` where `keep` says
    /// so, returning them.
    async fn cancel_launches(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        keep: &Keep,
    ) -> Result<Vec<QueueEntry>> {
        match keep {
            Keep::LastSessions {
                launches: QueuedLaunches::Cancel,
                ..
            } => self.cancel_waiting_launches(daemon, id).await,
            Keep::LastSessions { .. } | Keep::Records => Ok(Vec::new()),
        }
    }

    /// Stop the workspace `id` while its preparation waits, where nothing
    /// runs, cancelling the launches waiting there where `keep` says so and
    /// returning them; `None` once its preparation no longer waits.
    async fn stop_waiting(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        keep: &Keep,
    ) -> Result<Option<Vec<QueueEntry>>> {
        match keep {
            Keep::LastSessions {
                launches: QueuedLaunches::Cancel,
                ..
            } => {
                self.cancel_launches_while_preparation_waits(daemon, id)
                    .await
            }
            Keep::LastSessions { .. } | Keep::Records => {
                let id = id.to_owned();
                let records = self.records.clone();
                let waits = (self.store)
                    .run(move |db| records.preparation_waits(db, &id))
                    .await?;
                Ok(waits.map(|_| Vec::new()))
            }
        }
    }

    /// Stop the executions of the space `id` as `stopping` says and keep
    /// what they saved, returning the waiting launches it cancels.
    async fn stop_and_keep(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        stopping: Stopping<'_>,
    ) -> Result<Vec<QueueEntry>> {
        let Stopping {
            request,
            protected,
            policy,
            keep,
        } = stopping;
        // Before stopping, so an admission cannot start one after.
        let cancelled = self.cancel_launches(daemon, id, &keep).await?;
        // Before stopping, so a restore admitted meanwhile cannot start
        // after, and again once stopped, for the request of an agent a
        // signal was ending meanwhile.
        if protected == Protected::Stop {
            self.cancel_stopped_restores(daemon, id).await?;
        }
        self.stop_executions_keeping(daemon, id, policy, request, protected)
            .await?;
        if protected == Protected::Stop {
            self.cancel_stopped_restores(daemon, id).await?;
        }
        if let Keep::LastSessions { .. } = keep {
            (self.keep_stopped_as_last_sessions(daemon, &SpaceSelector::id(id))).await?;
        }
        Ok(cancelled)
    }

    /// Before the daemon exits, ask every connected execution to stop. Wrappers
    /// that can reattach detach instead and keep their commands running or
    /// their wait for an agent's restore; the rest stop as `shoal stop` does, so a restart leaves resumable records
    /// instead of disconnected executions. Requests already accepted cannot
    /// register afterwards, and wrappers that miss the bound are left for the
    /// startup audit. Returns the spaces of the executions that stopped.
    pub async fn stop_for_shutdown(&self) -> HashSet<SpaceId> {
        let deadline = Instant::now() + timing::WORKSPACE_STOP_TIMEOUT;
        let stopping = self
            .executions
            .shut_down(StopRequest {
                reason: "the Shoal daemon shut down".into(),
                save: Save::Everything,
            })
            .await;
        while Instant::now() < deadline && self.executions.has_connections().await {
            sleep(timing::WORKSPACE_STOP_POLL_INTERVAL).await;
        }
        stopping
    }

    pub async fn stop_disconnected(
        &self,
        daemon: &dyn Daemon,
        execution: &Execution,
        policy: StopPolicy,
    ) -> Result<()> {
        let processes = self.execution_processes(daemon, execution).await?;
        processes.stop().await?;
        let after = self.execution_processes(daemon, execution).await?;
        ensure!(
            after.owned.is_empty(),
            "processes of execution {} survived stopping: {}",
            execution.id,
            pids(&after.owned)
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
            self.ensure_directory_unused(daemon, &execution.space_id)
                .await?;
        }
        self.forget_execution(daemon, &execution.id).await
    }

    /// The processes of `execution` now. What runs below another
    /// execution's agent wrapper, such as the agent a `start --here` among
    /// them runs, is that agent's, though it carries this execution's marker.
    async fn execution_processes(
        &self,
        daemon: &dyn Daemon,
        execution: &Execution,
    ) -> Result<Processes> {
        let scan = process::scan(HashSet::from([execution.id.clone()])).await?;
        self.inspect_execution(daemon, execution, &scan).await
    }

    /// Fail while processes use the worktree of the space `id`: an
    /// acknowledgement that an unprovable execution ended cannot cover a
    /// visible process. A chat's directory is another's, which it does not
    /// tell apart.
    async fn ensure_directory_unused(&self, daemon: &dyn Daemon, id: &SpaceId) -> Result<()> {
        let space = daemon.space(&SpaceSelector::id(id)).await?;
        let Some(workspace) = space.worktree() else {
            return Ok(());
        };
        let path = workspace.path.clone();
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

    /// Ask connected wrappers to stop and signal disconnected survivors.
    pub async fn stop_executions(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        policy: StopPolicy,
        request: &StopRequest,
    ) -> Result<()> {
        self.stop_executions_keeping(daemon, id, policy, request, Protected::Stop)
            .await
    }

    /// Wrappers decide what to save; setup and merge save nothing. The
    /// agent of a chat that became the workspace `id` is handed over first.
    async fn stop_executions_keeping(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        policy: StopPolicy,
        request: &StopRequest,
        protected: Protected,
    ) -> Result<()> {
        daemon.hand_over_before_removal(id).await?;
        self.stop_recorded_executions(daemon, id, policy, request, protected)
            .await
    }

    /// Stop the agent of the chat that became the workspace `id`, which a
    /// conversion hands over, an execution of the space it ran in.
    pub async fn stop_space_agent_executions(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        request: &StopRequest,
    ) -> Result<()> {
        let policy = StopPolicy::RequireCompleteProof;
        (self.stop_recorded_executions(daemon, id, policy, request, Protected::Stop)).await
    }

    /// [`Self::stop_executions_keeping`] of the executions recorded now.
    async fn stop_recorded_executions(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
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
                .run(move |db| shoal_store::executions(db, &query_id))
                .await?;
            if executions.is_empty() {
                return Ok(());
            }
            let mut waiting = Vec::new();
            let mut disconnected = Vec::new();
            for execution in executions {
                if !connections.is_connected(&execution.id) {
                    disconnected.push(execution);
                } else if self
                    .stop_connected(&connections, &execution.id, request, protected)
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
                self.stop_disconnected(daemon, &execution, policy).await?;
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

    /// Wait until the execution `id` of the space `space`, whose wrapper
    /// was asked to stop, ends, as a space's stop waits for its executions.
    pub async fn await_execution_end(&self, space: &SpaceId, id: &ExecutionId) -> Result<()> {
        let deadline = Instant::now() + timing::WORKSPACE_STOP_TIMEOUT;
        loop {
            let query_id = space.to_owned();
            let executions = self
                .store
                .run(move |db| shoal_store::executions(db, &query_id))
                .await?;
            let Some(execution) = executions.iter().find(|execution| execution.id == *id) else {
                return Ok(());
            };
            ensure!(
                Instant::now() < deadline,
                "timed out waiting for the agent's wrapper to stop: {}",
                self.stop_progress(execution)
            );
            sleep(timing::WORKSPACE_STOP_POLL_INTERVAL).await;
        }
    }

    /// Ask the connected wrapper of `id` to stop for `request`, cancelling
    /// its agent's automatic restore, unless `protected` keeps an agent that
    /// protection stopped waiting for its restore. Returns whether the
    /// wrapper was asked.
    async fn stop_connected(
        &self,
        connections: &Connections<'_>,
        id: &ExecutionId,
        request: &StopRequest,
        protected: Protected,
    ) -> Result<bool> {
        if self.agent_overload(id).await.is_some() {
            if protected == Protected::Keep {
                return Ok(false);
            }
            self.forget_agent_stop(id).await;
        }
        connections.stop(id, request).await
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
