//! Registering and tracking executions. Terminal I/O stays in the execution
//! wrapper.
pub mod completion;
pub mod homes;
pub mod reregistration;
pub mod stopping;

use crate::Agents;
use crate::config::repo::Cache;
use crate::daemon::Daemon;
use crate::hooks::HookKind;
use crate::log;
use crate::model::CopiedFrom;
use crate::model::CopyIgnored;
use crate::model::Execution;
use crate::model::ExecutionPlan;
use crate::model::MergePlan;
use crate::model::PortReservation;
use crate::model::Repository;
use crate::model::Space;
use crate::model::UnmergePlan;
use crate::model::Workspace;
use crate::places::ExecutionPlace;
use crate::process::execution::Processes;
use crate::process::identity as process;
use crate::process::identity::Identity;
use crate::protocol::Reattach;
use crate::protocol::timing;
use crate::refusal::{Reason, Refusal};
use crate::state::ExecutionState;
use crate::state::WorkspaceState;
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{OptionalExtension as _, Transaction, params};
use shoal_daemon_base::callers::Caller;
use shoal_daemon_base::gates::GuardMode;
use shoal_daemon_base::gates::ResourceGuard;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::demos::Launch;
use shoal_protocol::model::ByKind;
use shoal_protocol::scope_tokens::ScopeToken;
use shoal_protocol::space_names::SpaceSelector;
use shoal_store;
use shoal_store::lifecycle::Generation;
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::sync::{OwnedMutexGuard, watch};

pub mod reservation;
pub mod state;

pub use completion::agent_execution_space;
pub use reservation::Reserved;
pub use shoal_protocol::executions::ExecutionKind;
pub use state::{Completing, Connections, Executions};
pub use stopping::StopPolicy;
pub use stopping::{Save, SpaceStop, StopRequest, UnprovenStop};

/// A restored connection for a wrapper whose command kept running.
#[derive(Debug)]
pub struct ReattachedExecution {
    pub space: Space,
    pub stop: watch::Receiver<bool>,
}

/// The connection must retain the Git guard through execution completion.
#[derive(Debug)]
pub struct StartedExecution {
    pub plan: ExecutionPlan,
    /// The space it runs in, as the registration read it.
    pub space: Space,
    pub stop: watch::Receiver<bool>,
    pub _git_guard: Option<OwnedMutexGuard<()>>,
}

/// What a wrapper asks of its execution besides its kind.
#[derive(Default)]
pub struct ExecutionRequest {
    /// The demo the command runs.
    pub demo: Option<Launch>,
    /// Where it runs in a terminal host.
    pub place: Option<ExecutionPlace>,
    /// How a merge runs.
    pub merge: Option<crate::model::MergeOptions>,
    /// Whether it runs a tracked agent, as a chat's one execution does.
    pub agent: bool,
}

#[derive(Default)]
struct PreparedExecution {
    setup_cmd: Option<PathBuf>,
    copy_ignored: Option<CopyIgnored>,
    pre_setup: Option<PathBuf>,
    registration_guard: Option<OwnedMutexGuard<()>>,
    lifetime_guard: Option<OwnedMutexGuard<()>>,
    merge: Option<Box<MergePlan>>,
    unmerge: Option<Box<UnmergePlan>>,
    _resources: Option<ResourceGuard>,
}

/// The record a new execution persists at registration.
#[derive(Clone)]
struct ExecutionRecord {
    id: ExecutionId,
    kind: ExecutionKind,
    /// Whether it runs a tracked agent.
    agent: bool,
    parent_execution: Option<ExecutionId>,
    /// The agent execution whose process tree registers it.
    agent_execution: Option<ExecutionId>,
    wrapper: Option<Identity>,
    demo: Option<Launch>,
    place: Option<ExecutionPlace>,
}

impl ExecutionRecord {
    /// Whether its start waits out a lifecycle operation that passes. A
    /// tracked agent's does not: its registration holds the agent queue's
    /// admission, which a stop of its workspace needs.
    fn waits_out_reservations(&self) -> bool {
        self.kind != ExecutionKind::Setup && !self.agent
    }
}

/// The directories a space's processes use: a workspace's repository's
/// caches and the space's temporary directory.
pub struct ProcessDirectories {
    caches: BTreeMap<String, PathBuf>,
    tmp_dir: Option<PathBuf>,
}

impl ProcessDirectories {
    /// The directories of `workspace_id`'s processes, created when missing.
    fn prepare(
        repository: &Repository,
        caches: &BTreeMap<String, Cache>,
        workspace_id: &SpaceId,
    ) -> Result<Self> {
        Ok(Self {
            caches: crate::caches::directories(repository, caches)?,
            tmp_dir: Some(crate::tmp_dirs::directory(repository, workspace_id)?),
        })
    }

    /// The directories of a chat's processes: its repository's caches,
    /// created when missing, none without one, and `tmp_dir`.
    fn of_chat(
        repository: Option<&Repository>,
        caches: &BTreeMap<String, Cache>,
        tmp_dir: Option<PathBuf>,
    ) -> Result<Self> {
        let caches = match repository {
            Some(repository) => crate::caches::directories(repository, caches)?,
            None => BTreeMap::new(),
        };
        Ok(Self { caches, tmp_dir })
    }
}

struct RegisteredExecution {
    id: ExecutionId,
    space: Space,
    plan_space: crate::model::ExecutionSpace,
    scope_token: ScopeToken,
    ports: Vec<PortReservation>,
    environment: std::collections::BTreeMap<String, std::ffi::OsString>,
    stop: watch::Receiver<bool>,
}

/// Operations on the client type [`ExecutionKind`] that only the daemon uses.
pub trait ExecutionKindExt {
    /// Apply the kind's lifecycle checks in the registration transaction.
    fn reserve(
        self,
        tx: &Transaction<'_>,
        records: &dyn crate::daemon::Records,
        workspace_id: &SpaceId,
        parent_execution: Option<&ExecutionId>,
    ) -> Result<()>;
}

impl ExecutionKindExt for ExecutionKind {
    fn reserve(
        self,
        tx: &Transaction<'_>,
        records: &dyn crate::daemon::Records,
        workspace_id: &SpaceId,
        parent_execution: Option<&ExecutionId>,
    ) -> Result<()> {
        // Quarantine may have left a moving workspace failed, which setup
        // would otherwise accept.
        shoal_store::ensure_no_pending_move(tx, workspace_id)?;
        match self {
            Self::Command | Self::Merge | Self::Unmerge => {
                reservation::ensure_unreserved(tx, workspace_id)?;
                shoal_store::require_ready(tx, workspace_id)?
            }
            Self::Setup => {
                let blocking = other_executions(tx, Some(workspace_id), parent_execution)?;
                if let Some((_, blocking)) = blocking.into_iter().next() {
                    bail!(
                        "workspace has active or unknown execution {blocking}; setup is unavailable until it finishes or is cleared"
                    );
                }
                ensure!(
                    records.reserve_setup(tx, workspace_id)?,
                    "workspace is busy"
                );
            }
        }
        Ok(())
    }
}

impl Agents {
    /// Register an execution and issue its scope token. The returned receiver
    /// fires when the workspace asks its commands to stop.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn begin_execution(
        &self,
        daemon: &dyn Daemon,
        selector: &str,
        wrapper: Option<Identity>,
        kind: ExecutionKind,
        caller: Option<&Caller>,
    ) -> Result<StartedExecution> {
        self.begin_execution_running(daemon, selector, wrapper, kind, caller, None)
            .await
    }

    /// Register an execution and issue its scope token, recording the demo it
    /// runs with it. The returned receiver fires when the workspace asks its
    /// commands to stop.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn begin_execution_running(
        &self,
        daemon: &dyn Daemon,
        selector: &str,
        wrapper: Option<Identity>,
        kind: ExecutionKind,
        caller: Option<&Caller>,
        demo: Option<Launch>,
    ) -> Result<StartedExecution> {
        let request = ExecutionRequest {
            demo,
            ..Default::default()
        };
        self.begin_execution_placed(
            daemon,
            &SpaceSelector::from(selector),
            wrapper,
            kind,
            caller,
            request,
        )
        .await
    }

    /// Register the execution of an agent in the space `selector` names, a
    /// workspace's or a chat's, with its scope token, as its wrapper does
    /// before it tracks the agent.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn begin_agent_execution(
        &self,
        daemon: &dyn Daemon,
        selector: &str,
    ) -> Result<StartedExecution> {
        let request = ExecutionRequest {
            agent: true,
            ..Default::default()
        };
        let selector = SpaceSelector::from(selector);
        self.begin_execution_placed(
            daemon,
            &selector,
            None,
            ExecutionKind::Command,
            None,
            request,
        )
        .await
    }

    /// End every execution of the space `id` as exited, as their wrappers
    /// report when they exit.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn end_executions(&self, daemon: &dyn Daemon, id: &SpaceId) {
        let space = id.clone();
        let executions = self
            .store
            .run(move |db| {
                let mut statement = db.prepare("SELECT id FROM executions WHERE space_id=?1")?;
                let rows = statement.query_map([&space], |row| row.get::<_, ExecutionId>(0))?;
                Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .await
            .unwrap();
        for execution in executions {
            self.end_test_execution(daemon, execution).await;
        }
    }

    /// End execution `id` as its wrapper does once its command exits: the
    /// process group it recorded ends, then it reports the exit, which the
    /// space's log records for an agent.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn end_test_execution(&self, daemon: &dyn Daemon, id: ExecutionId) {
        let query = id.clone();
        let recorded = self
            .store
            .run(move |db| shoal_store::find_execution(db, &query))
            .await
            .unwrap();
        if let Some(execution) = &recorded
            && let Some(name) = execution.agent.clone()
        {
            let ended = shoal_daemon_base::events::AgentEvent {
                source: None,
                name,
                execution_id: Some(id.clone()),
                exit_code: Some(0),
                oom_kills: 0,
            };
            let kind = shoal_daemon_base::events::EventKind::AgentExited;
            shoal_daemon_base::events::record_agent(&self.store, &execution.space_id, kind, ended)
                .await;
        }
        if let Some(group) = recorded.and_then(|execution| execution.group_id) {
            // SAFETY: kill only sends a signal.
            unsafe { libc::kill(-(group as libc::pid_t), libc::SIGKILL) };
            while process::group_alive(group).await.unwrap() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
        self.finish_execution(daemon, id, ExecutionKind::Command, Some(0))
            .await
            .unwrap();
    }

    /// Register an execution and issue its scope token, recording the demo it
    /// runs, which takes over the lease its preset acquired, and the place it
    /// runs at with it. The returned receiver fires when the workspace asks
    /// its commands to stop.
    pub async fn begin_execution_placed(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        wrapper: Option<Identity>,
        kind: ExecutionKind,
        caller: Option<&Caller>,
        request: ExecutionRequest,
    ) -> Result<StartedExecution> {
        let ExecutionRequest {
            demo,
            place,
            merge,
            agent,
        } = request;
        // An agent's execution is its own, wherever the process that started
        // it runs.
        let agent_execution = match caller.filter(|_| !agent) {
            Some(caller) => self.caller_agent(caller).await.map(|agent| agent.execution),
            None => None,
        };
        let launch = demo.clone();
        let record = ExecutionRecord {
            id: ExecutionId::generate(),
            kind,
            agent,
            parent_execution: caller.and_then(Caller::execution_id).cloned(),
            agent_execution,
            wrapper,
            demo,
            place,
        };
        let (space_id, preparation, registered) = self
            .register_unreserved(daemon, selector, record, caller, merge)
            .await?;
        let PreparedExecution {
            setup_cmd,
            copy_ignored,
            pre_setup,
            registration_guard,
            lifetime_guard,
            merge,
            unmerge,
            _resources,
        } = preparation;
        if let Some(launch) = &launch {
            daemon
                .give_demo_its_leases(&registered.id, &space_id, launch)
                .await;
        }
        // Setup releases its Git gate before the hook; resources remain guarded
        // until pre-setup completes, while merge's gate travels with the plan.
        drop(registration_guard);
        self.run_pre_setup(daemon, &registered, kind, pre_setup.as_deref())
            .await?;
        Ok(StartedExecution {
            plan: ExecutionPlan {
                id: registered.id,
                space: registered.plan_space,
                scope_token: registered.scope_token,
                setup_cmd,
                copy_ignored,
                ports: registered.ports,
                environment: registered.environment,
                merge,
                unmerge,
                // Only the runner the daemon launched, not a step's command.
                reports_signals: caller.is_none() && daemon.runs_job(&space_id).await,
            },
            space: registered.space,
            stop: registered.stop,
            _git_guard: lifetime_guard,
        })
    }

    /// Prepare `record` in the space `selector` names and record it there,
    /// waiting out a lifecycle operation that holds the workspace for a
    /// while, such as an update an agent's start makes, and preparing it
    /// again for the workspace that operation leaves, also when one came and
    /// went while it registered.
    async fn register_unreserved(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        record: ExecutionRecord,
        caller: Option<&Caller>,
        merge: Option<crate::model::MergeOptions>,
    ) -> Result<(SpaceId, PreparedExecution, RegisteredExecution)> {
        loop {
            let (space_id, seen, preparation) = self
                .prepare_execution(daemon, selector, &record, caller, merge.clone())
                .await?;
            let error = match self
                .register_execution(daemon, &space_id, record.clone())
                .await
            {
                Ok(registered) => return Ok((space_id, preparation, registered)),
                Err(error) => error,
            };
            if !self
                .reservation_refused(daemon, &error, &space_id, seen)
                .await?
            {
                return Err(error);
            }
            if !record.waits_out_reservations() {
                return Err(Refusal::wrap(Reason::NotReady, error));
            }
            // The operation may need the gates the preparation holds.
            drop(preparation);
            self.await_unreserved(daemon, &space_id).await?;
        }
    }

    /// The lifecycle generation of the workspace `id` at the start's first
    /// look, once no lifecycle operation that passes holds it where `record`
    /// waits those out.
    async fn first_look(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        record: &ExecutionRecord,
    ) -> Result<Generation> {
        if record.waits_out_reservations() {
            self.await_unreserved(daemon, id).await
        } else {
            daemon.lifecycle_generation(id).await
        }
    }

    /// Wait until no lifecycle operation that passes holds the workspace
    /// `id`, while the daemon accepts registrations: its own stop holds
    /// workspaces until the work it stops ends.
    async fn await_unreserved(&self, daemon: &dyn Daemon, id: &SpaceId) -> Result<Generation> {
        let wanted = || self.executions.ensure_accepting();
        (daemon.await_reservation_end(id, &wanted)).await
    }

    /// Whether a lifecycle operation that passes refused the registration
    /// in `space_id` that failed with `error`: one its transaction found,
    /// one that holds the workspace now, or one that began and ended after
    /// the start's first look found the generation `seen`, any of which may
    /// have left the worktree unverifiable meanwhile.
    pub async fn reservation_refused(
        &self,
        daemon: &dyn Daemon,
        error: &anyhow::Error,
        space_id: &SpaceId,
        seen: Option<Generation>,
    ) -> Result<bool> {
        Ok(error.is::<Reserved>() || daemon.reserved_since(space_id, seen).await?)
    }

    /// Record `record` as an execution of the space `space_id`, with what
    /// its plan reads from the space.
    async fn register_execution(
        &self,
        daemon: &dyn Daemon,
        space_id: &SpaceId,
        record: ExecutionRecord,
    ) -> Result<RegisteredExecution> {
        let mut connections = self.executions.connections().await;
        connections.ensure_accepting()?;
        let home = self.execution_home(daemon, space_id).await?;
        let (id, kind, agent_execution) = (
            record.id.clone(),
            record.kind,
            record.agent_execution.clone(),
        );
        let (home, scope_token, provisions) = self.persist_execution(record, home).await?;
        let homes::Provisions { ports, directories } = provisions;
        let environment = home.environment(&ports, &directories);
        let receiver = connections
            .connect(
                &id,
                kind,
                home.id(),
                scope_token.clone(),
                agent_execution,
                false,
            )
            .await;
        Ok(RegisteredExecution {
            id,
            plan_space: home.plan_space(),
            space: home.into_space(),
            scope_token,
            ports,
            environment,
            stop: receiver,
        })
    }

    /// Reserve the execution's lifecycle, insert its running record and read
    /// its scope token and what its plan takes from `home` in one
    /// transaction, returning the space with that.
    async fn persist_execution(
        &self,
        record: ExecutionRecord,
        home: homes::Home,
    ) -> Result<(homes::Home, ScopeToken, homes::Provisions)> {
        let ExecutionRecord {
            id,
            kind,
            agent,
            parent_execution,
            agent_execution,
            wrapper,
            demo,
            place,
        } = record;
        // A place's ownership moves only under the lock its closes hold.
        let _ownership = match place {
            Some(_) => Some(self.place_ownership.lock().await),
            None => None,
        };
        let (caches, records) = (home.caches(), self.records.clone());
        self.store
            .write(move |tx| {
                let space_id = home.id();
                home.reserve(tx, &*records, kind, agent, parent_execution.as_ref())?;
                // Under the write lock emptying caches takes, and undone with
                // the execution's row when the directories cannot be prepared.
                let provisions = home.provide(tx, &caches)?;
                let scope_token = home.scope_token(tx)?;
                // Only commands may reattach: setup and merge hold daemon
                // gates that a restart releases.
                let reattach_token = (kind == ExecutionKind::Command).then_some(&scope_token);
                tx.execute(
                    "INSERT INTO executions(id,space_id,state,wrapper,scope_token,agent_execution)
                        VALUES (?1,?2,?3,?4,?5,?6)",
                    params![
                        id,
                        space_id,
                        ExecutionState::Running,
                        shoal_store::json_text(wrapper.as_ref())?,
                        reattach_token,
                        agent_execution
                    ],
                )?;
                if let Some(demo) = &demo {
                    records.record_demo(tx, &id, space_id, demo)?;
                }
                if let Some(place) = &place {
                    crate::places::record_execution_place(tx, &id, space_id, place)?;
                }
                Ok((home, scope_token, provisions))
            })
            .await
    }

    async fn run_pre_setup(
        &self,
        daemon: &dyn Daemon,
        registered: &RegisteredExecution,
        kind: ExecutionKind,
        pre_setup: Option<&Path>,
    ) -> Result<()> {
        // Only a workspace's setup has a pre-setup hook.
        let (Some(command), Some(workspace)) = (pre_setup, registered.space.to_workspace()) else {
            return Ok(());
        };
        let workspace = &workspace;
        if let Err(error) = async {
            crate::hooks::run_detached(
                crate::hooks::Hook::PreSetup,
                workspace,
                command,
                &self.paths,
                &registered.environment,
            )
            .await?;
            daemon.verify_worktree(workspace).await
        }
        .await
        {
            self.finish_execution(daemon, registered.id.clone(), kind, Some(1))
                .await?;
            daemon
                .set_state(
                    &workspace.id,
                    WorkspaceState::Failed,
                    Some(format!("{error:#}")),
                )
                .await?;
            return Err(error);
        }
        Ok(())
    }

    /// Prepare `record` in the space `selector` names, returning that space
    /// with its workspace's lifecycle generation at the start's first look.
    async fn prepare_execution(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        record: &ExecutionRecord,
        caller: Option<&Caller>,
        merge: Option<crate::model::MergeOptions>,
    ) -> Result<(SpaceId, Option<Generation>, PreparedExecution)> {
        if let Some(wrapper) = &record.wrapper {
            ensure!(
                process::alive(wrapper)?,
                "execution wrapper is no longer alive"
            );
        }
        let preparation = match daemon.space(selector).await?.by_kind() {
            ByKind::Workspace(workspace) => {
                // An operation that passes, such as a reclamation, may leave
                // the worktree unverifiable until it ends; registration
                // checks again in its transaction, and a failed one compares
                // the generation with this.
                let seen = self.first_look(daemon, &workspace.id, record).await?;
                let preparation = self
                    .prepare_execution_kind(daemon, &workspace, record.kind, caller, merge)
                    .await?;
                (workspace.id, Some(seen), preparation)
            }
            // A chat runs only its agent, which its registration checks, with
            // nothing to prepare.
            ByKind::Chat(chat) => (chat.id, None, PreparedExecution::default()),
        };
        Ok(preparation)
    }

    /// `merge` says how a merge runs, which its review gate may refuse
    /// `caller` first; a scoped `caller`'s merge or unmerge needs its
    /// permission.
    async fn prepare_execution_kind(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        kind: ExecutionKind,
        caller: Option<&Caller>,
        merge: Option<crate::model::MergeOptions>,
    ) -> Result<PreparedExecution> {
        match kind {
            ExecutionKind::Command => Ok(PreparedExecution::default()),
            ExecutionKind::Merge => {
                let guard = self.gates.lock_git(workspace.repository()).await;
                let merge = daemon
                    .prepare_merge(
                        &SpaceSelector::id(&workspace.id),
                        &merge.unwrap_or_default(),
                        caller,
                    )
                    .await;
                let merge = match merge {
                    Ok(merge) => merge,
                    // Queueing the review a gate missed takes the registry
                    // gate, which comes before the Git gate.
                    Err(error) => {
                        drop(guard);
                        return Err(daemon.queue_unreviewed(error).await);
                    }
                };
                if let Some(caller) = caller {
                    daemon.authorize_scoped_merge(&merge, caller).await?;
                }
                Ok(PreparedExecution {
                    merge: Some(Box::new(merge)),
                    lifetime_guard: Some(guard),
                    ..Default::default()
                })
            }
            ExecutionKind::Unmerge => {
                let guard = self.gates.lock_git(workspace.repository()).await;
                let unmerge = daemon
                    .prepare_unmerge(&SpaceSelector::id(&workspace.id))
                    .await?;
                if let Some(caller) = caller {
                    daemon.authorize_scoped_unmerge(&unmerge, caller).await?;
                }
                Ok(PreparedExecution {
                    unmerge: Some(Box::new(unmerge)),
                    lifetime_guard: Some(guard),
                    ..Default::default()
                })
            }
            ExecutionKind::Setup => {
                // Not before the seed build, which creation applies last.
                (self.gates)
                    .wait_for_creation(workspace.repository(), &workspace.id)
                    .await;
                let git_guard = self.gates.lock_git(workspace.repository()).await;
                daemon.ensure_no_pending_rename(&workspace.id).await?;
                let setup_cmd = daemon.workspace_hook(workspace, HookKind::Setup).await?;
                let pre_setup = daemon.workspace_hook(workspace, HookKind::PreSetup).await?;
                // A copy of a workspace's files has that workspace's ignored
                // files instead.
                let setup = &daemon.workspace_settings(workspace).await?.setup;
                let copy_ignored = (setup.copy_ignored
                    && !(workspace.copied_from.as_ref()).is_some_and(CopiedFrom::took_files))
                .then(|| CopyIgnored {
                    exclude: setup.copy_ignored_exclude.clone(),
                });
                // One that skipped setup runs it to clear that and run the
                // hooks around it, whatever the setup does.
                ensure!(
                    setup_cmd.is_some()
                        || pre_setup.is_some()
                        || copy_ignored.is_some()
                        || workspace.setup_skipped,
                    "no setup configured; set setup.command, hooks.pre_setup or setup.copy_ignored"
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
        id: ExecutionId,
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
        let records = self.records.clone();
        self.store
            .write(move |tx| {
                let updated = tx.execute(
                    "UPDATE executions SET child=?2,group_id=?3 WHERE id=?1 AND group_id IS NULL",
                    params![id, shoal_store::json_text(child.as_ref())?, group_id],
                )?;
                ensure!(
                    updated == 1,
                    "execution already registered or no longer exists"
                );
                records.mark_demo_run(tx, &id)?;
                // A restored agent takes over the slot its restore kept.
                tx.execute("DELETE FROM agent_queue WHERE execution_id=?1", [&id])?;
                Ok(())
            })
            .await
    }

    /// Forget the exited child of an execution whose wrapper starts another,
    /// once what it left running is proven stopped, as a paused agent's is,
    /// and the space the next run starts in is still where it was recorded.
    /// Unproven survivors keep the child's identity, so the next run cannot
    /// register over the evidence.
    pub async fn release_exited_child(
        &self,
        daemon: &dyn Daemon,
        id: &ExecutionId,
        space: &SpaceId,
    ) -> Result<()> {
        match self
            .pause_agent_execution(daemon, id, space, timing::PROCESS_PROOF_TIMEOUT)
            .await?
        {
            crate::control::Pause::Paused => {}
            crate::control::Pause::Unproven(issue) => {
                bail!("the previous run left processes Shoal cannot prove stopped: {issue}")
            }
        }
        self.execution_home(daemon, space).await.map(drop)
    }

    /// Close an execution. `exit_code` is `None` when the wrapper disconnected
    /// without reporting; such executions stay recorded as unknown. Returns
    /// whether the command's processes are verifiably gone. The reporting
    /// wrapper remains alive until its completion acknowledgement.
    pub async fn finish_execution(
        &self,
        daemon: &dyn Daemon,
        id: ExecutionId,
        kind: ExecutionKind,
        exit_code: Option<i32>,
    ) -> Result<bool> {
        self.finish_execution_scanned(daemon, id, kind, exit_code, None)
            .await
    }

    /// [`Self::finish_execution`], deciding completion from `cleared`, where
    /// given: a scan taken since the command reported its exit that found
    /// nothing of it left, and stopped nothing.
    pub async fn finish_execution_scanned(
        &self,
        daemon: &dyn Daemon,
        id: ExecutionId,
        kind: ExecutionKind,
        exit_code: Option<i32>,
        cleared: Option<process::Scan>,
    ) -> Result<bool> {
        self.forget_proxy_reports(&id, exit_code.is_some()).await;
        let scan = match (exit_code, cleared) {
            (None, _) => None,
            (Some(_), Some(cleared)) => Some(Ok(cleared)),
            (Some(_), None) => Some(self.completion_scan(&id).await?),
        };
        self.finish_execution_after_scan(daemon, id, kind, exit_code, scan)
            .await
    }

    /// Scan for the processes of the execution `id`, which reported its exit.
    async fn completion_scan(&self, id: &ExecutionId) -> Result<Result<process::Scan>> {
        // Daemon-side reservations can be registered without a wrapper while
        // a command is prepared. There is no process ownership to reconcile
        // in that state, and a whole-process scan on macOS can observe an
        // unrelated process whose environment is disappearing. Skip that
        // inventory until a wrapper, child, or process group has been
        // recorded.
        let query_id = id.clone();
        let needs_scan = self
            .store
            .run(move |db| {
                Ok(
                    shoal_store::find_execution(db, &query_id)?.is_none_or(|execution| {
                        execution.wrapper.is_some()
                            || execution.child.is_some()
                            || execution.group_id.is_some()
                    }),
                )
            })
            .await?;
        Ok(if needs_scan {
            process::scan(HashSet::from([id.clone()])).await
        } else {
            Ok(process::Scan::default())
        })
    }

    pub async fn finish_execution_after_scan(
        &self,
        daemon: &dyn Daemon,
        id: ExecutionId,
        kind: ExecutionKind,
        exit_code: Option<i32>,
        scan: Option<Result<process::Scan>>,
    ) -> Result<bool> {
        let issue = self.execution_completion_issue(daemon, &id, scan).await?;
        let complete = issue.is_none();
        self.keep_stopped_turn(&id, None).await;
        // Before its record goes, so a demo that replaces it waits for them.
        if complete {
            daemon.release_execution_leases(&id).await;
        }
        let connections = self.executions.connections().await;
        let completed = self
            .record_execution_completion(&id, kind, exit_code, issue)
            .await?;
        daemon.ask_user_about(&completed.released).await;
        if let Some(space) = &completed.agent_space {
            daemon.settle_agent(space);
            // An agent that comes back, asleep or restarting, keeps them.
            if complete && let Err(error) = daemon.release_ended_leases_of(space).await {
                log!("leases of the agent of {space} not released: {error:#}");
            }
        }
        let demo = completed.demo;
        if connections.disconnect(&id).await {
            self.agent_queue.wake();
        }
        if !complete {
            self.signals.execution_became_unknown();
        }
        self.close_demo_session(daemon, demo).await;
        daemon.wake_runner_for_ended_job(&id).await;
        Ok(complete)
    }

    /// Restore the connection of a wrapper that kept its command running while
    /// the daemon was away. The record, the scope token issued at launch and
    /// the wrapper and child identities must all match.
    pub async fn reattach_execution(
        &self,
        daemon: &dyn Daemon,
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
        let (workspace_id, agent_execution, narrowing) =
            self.authorize_reattachment(request).await?;
        let space = daemon.space_by_id(&workspace_id).await?;
        // Before protection can know the agent again and save a new stop.
        // A wrapper awaiting a restore has received its stop.
        if request.awaiting_restore.is_none() {
            self.discard_undelivered_stop(&space, &request.execution);
        }
        // A stopped agent stays stopped until its restore.
        let stop = connections
            .connect(
                &request.execution,
                ExecutionKind::Command,
                &space.id,
                request.scope_token.clone(),
                agent_execution,
                request.awaiting_restore.is_some(),
            )
            .await;
        drop(connections);
        // A restore whose agent runs again ended, even if the previous daemon
        // stopped before removing it.
        if request.awaiting_restore.is_none() {
            self.remove_restore(&request.execution).await;
            self.forget_agent_stop(&request.execution).await;
        }
        if let Some(name) = &request.agent {
            let agent = shoal_protocol::executions::TrackedAgent {
                name: name.clone(),
                model: request.model.clone(),
                mode: (request.mode.clone()).unwrap_or(crate::config::modes::ModeName::DEFAULT),
                // What its launch recorded, which a reattaching wrapper
                // cannot change.
                narrowing,
                new_session: false,
            };
            self.track_agent(
                daemon,
                &request.execution,
                &agent,
                &space.id,
                &space.name,
                request.recover,
                Duration::from_millis(request.running_ms),
            )
            .await;
            match &request.awaiting_restore {
                Some(protection) => {
                    (self.await_agent_restore(&request.execution, protection.clone())).await
                }
                None => self.restart_undelivered(&request.execution).await,
            }
        }
        daemon.touch(&space.id).await;
        // The reports its hooks left while the daemon was away wait for it.
        self.signals.wake_lost_reports();
        Ok(ReattachedExecution { space, stop })
    }

    /// Check a reattaching wrapper against its execution record and the scope
    /// token issued at launch, then mark the execution running, returning its
    /// workspace ID and the agent execution whose process tree registered it.
    /// The caller holds the connections lock until it connects the wrapper,
    /// so no other wrapper connects in between.
    async fn authorize_reattachment(
        &self,
        request: &Reattach,
    ) -> Result<(
        SpaceId,
        Option<ExecutionId>,
        crate::config::permissions::PermissionsConfig,
    )> {
        let request = request.clone();
        self.store
            .write(move |tx| {
                let execution = shoal_store::find_execution(tx, &request.execution)?
                    .context("execution record is missing")?;
                let (token, agent_execution): (Option<ScopeToken>, Option<ExecutionId>) = tx
                    .query_row(
                        "SELECT scope_token,agent_execution FROM executions WHERE id=?1",
                        [&request.execution],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )?;
                ensure!(
                    token.as_ref() == Some(&request.scope_token),
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
                Ok((execution.space_id, agent_execution, execution.narrowing))
            })
            .await
    }

    /// A reattaching wrapper still runs its command, so a stop the previous
    /// daemon saved but never delivered no longer describes the agent.
    fn discard_undelivered_stop(&self, space: &Space, execution: &ExecutionId) {
        match crate::execution::recovery::discard_undelivered(&self.paths, &space.id, execution) {
            Ok(true) => log!(
                "discarded an undelivered stop for execution {execution} in {}",
                &space.name
            ),
            Ok(false) => {}
            Err(error) => {
                log!("undelivered stop for execution {execution} not discarded: {error:#}")
            }
        }
    }

    /// Drop a wrapper's connection without recording completion, so its
    /// execution awaits reattachment.
    pub async fn detach_execution(&self, id: &ExecutionId) {
        if self.executions.disconnect(id).await {
            self.agent_queue.wake();
        }
    }

    async fn execution_completion_issue(
        &self,
        daemon: &dyn Daemon,
        id: &ExecutionId,
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
            .run(move |db| shoal_store::find_execution(db, &query_id))
            .await?;
        let Some(execution) = execution else {
            return Ok(Some("execution record is missing".into()));
        };
        Ok(
            match self.inspect_execution(daemon, &execution, &scan).await {
                Ok(processes) => processes.completion_issue(),
                Err(error) => Some(format!("process inspection failed: {error:#}")),
            },
        )
    }

    /// The record of the execution `id`, while it exists.
    pub async fn execution(&self, id: &ExecutionId) -> Result<Option<Execution>> {
        let id = id.to_owned();
        (self.store)
            .read(move |db| shoal_store::find_execution(db, &id))
            .await
    }

    /// The scope token the commands of the execution `id` act under.
    pub async fn execution_scope_token(&self, id: &ExecutionId) -> Result<ScopeToken> {
        let id = id.to_owned();
        let token = (self.store).read(move |db| shoal_store::scope_token(db, &id));
        token.await?.context("the execution has no scope token")
    }

    /// Combine the process evidence of `execution` from `scan`.
    pub async fn inspect_execution(
        &self,
        daemon: &dyn Daemon,
        execution: &Execution,
        scan: &process::Scan,
    ) -> Result<Processes> {
        let space = daemon
            .space(&SpaceSelector::id(&execution.space_id))
            .await?;
        let directory = self.space_directory(&space).await?;
        let agents = self.agent_wrappers().await?;
        Processes::inspect(execution, &directory, scan, &agents).await
    }

    /// The wrappers of the executions that run agents, each of which owns
    /// its agent wherever the process that started it runs.
    pub async fn agent_wrappers(&self) -> Result<Vec<process::Identity>> {
        self.store.read(|db| shoal_store::agent_wrappers(db)).await
    }

    /// What the agents of executions run now, which is theirs wherever the
    /// process that started them runs.
    pub async fn agent_trees(&self) -> Result<process::AgentTrees> {
        process::AgentTrees::of(&self.agent_wrappers().await?).await
    }

    /// Delete an execution record whose processes the caller proved stopped.
    pub async fn forget_execution(&self, daemon: &dyn Daemon, id: &ExecutionId) -> Result<()> {
        // A demo's viewer spawning meanwhile waits, then finds no record to
        // join and stops itself.
        let spawning = daemon.viewer_spawn_lock(id);
        let _spawning = spawning.lock().await;
        daemon.retire_execution_viewer(id).await?;
        self.keep_stopped_turn(id, None).await;
        daemon.release_execution_leases(id).await;
        let demo = self.delete_execution_record(daemon, id).await?;
        self.close_demo_session(daemon, demo).await;
        Ok(())
    }
}

/// The executions, with their spaces, other than `own`, which a setup in
/// their space leaves running, of the space `space` or of every space.
pub fn other_executions(
    db: &rusqlite::Connection,
    space: Option<&SpaceId>,
    own: Option<&ExecutionId>,
) -> Result<Vec<(SpaceId, ExecutionId)>> {
    let mut query = db.prepare(
        "SELECT space_id, id FROM executions
         WHERE (?1 IS NULL OR space_id=?1) AND (?2 IS NULL OR id != ?2)",
    )?;
    let rows = query.query_map(params![space, own], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Mark the executions a stopped daemon left running as of unknown state,
/// which the next one resolves once it sees whether they still run.
pub fn interrupt_running(tx: &Transaction<'_>) -> Result<()> {
    tx.execute(
        "UPDATE executions SET state=?1 WHERE state=?2",
        [ExecutionState::Unknown, ExecutionState::Running],
    )?;
    Ok(())
}

/// Delete the executions of the space `id`, and the queue entries that
/// restore or opened them, as its kind changes under it.
pub fn forget_space(tx: &Transaction<'_>, id: &SpaceId) -> Result<()> {
    tx.execute("DELETE FROM executions WHERE space_id=?1", [id])?;
    tx.execute("DELETE FROM agent_queue WHERE space_id=?1", [id])?;
    Ok(())
}

/// Delete the executions of the space `id` that run no agent.
pub fn forget_commands(tx: &Transaction<'_>, id: &SpaceId) -> Result<()> {
    tx.execute(
        "DELETE FROM executions WHERE space_id=?1 AND agent IS NULL",
        [id],
    )?;
    Ok(())
}

/// The executions of the space `id` that run an agent, other than those
/// whose restore waits in the queue, which run none yet.
pub fn running_agents(db: &rusqlite::Connection, id: &SpaceId) -> Result<Vec<ExecutionId>> {
    use crate::state::{QueueKind, QueueState};
    Ok(db
        .prepare(
            "SELECT id FROM executions e WHERE space_id=?1 AND agent IS NOT NULL
                AND NOT EXISTS (SELECT 1 FROM agent_queue
                    WHERE kind=?2 AND state=?3 AND execution_id=e.id)",
        )?
        .query_map(params![id, QueueKind::Restore, QueueState::Queued], |row| {
            row.get(0)
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// The agent execution `id` runs, when it runs one.
pub fn execution_agent(
    db: &rusqlite::Connection,
    id: &ExecutionId,
) -> Result<Option<crate::agent::Agent>> {
    Ok(db
        .query_row("SELECT agent FROM executions WHERE id=?1", [id], |row| {
            row.get(0)
        })
        .optional()?
        .flatten())
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

#[cfg(test)]
mod tests;
