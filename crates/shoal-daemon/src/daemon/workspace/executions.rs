//! Daemon-side execution registration and tracking. Stopping executions
//! lives in [`stopping`]. Terminal I/O stays in crate::execution, in the
//! invoking CLI process.
mod completion;
mod homes;
mod reregistration;
mod stopping;

use super::{GuardMode, Manager, ResourceGuard, places::ExecutionPlace};
use crate::client::execution_ids::ExecutionId;
use crate::client::scope_tokens::ScopeToken;
use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceSelector;
use crate::daemon::log;
use crate::{
    config::repo::Cache,
    daemon::{scope::Caller, store},
    demos::Launch,
    hooks::HookKind,
    model::{
        CopiedFrom, CopyIgnored, Execution, ExecutionPlan, MergePlan, PortReservation,
        PresentSpace, Repository, UnmergePlan, Workspace,
    },
    process::{
        execution::Processes,
        identity::{self as process, Identity},
    },
    protocol::{Reattach, timing},
    state::{ExecutionState, WorkspaceState},
};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{OptionalExtension, Transaction, params};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::sync::{OwnedMutexGuard, watch};

mod state;

#[cfg(test)]
mod tests;

pub use crate::client::executions::ExecutionKind;
pub(crate) use state::{Completing, Connections, Executions};
pub(super) use stopping::StopPolicy;
pub use stopping::{Save, StopRequest, UnprovenStop};

/// A restored connection for a wrapper whose command kept running.
#[derive(Debug)]
pub(crate) struct ReattachedExecution {
    pub space: PresentSpace,
    pub stop: watch::Receiver<bool>,
}

/// The connection must retain the Git guard through execution completion.
#[derive(Debug)]
pub(crate) struct StartedExecution {
    pub plan: ExecutionPlan,
    /// The space it runs in, as the registration read it.
    pub space: PresentSpace,
    pub stop: watch::Receiver<bool>,
    pub _git_guard: Option<OwnedMutexGuard<()>>,
}

/// What a wrapper asks of its execution besides its kind.
#[derive(Default)]
pub(crate) struct ExecutionRequest {
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

/// The directories a space's processes use: a workspace's repository's
/// caches and the space's temporary directory.
struct ProcessDirectories {
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
            tmp_dir: crate::tmp_dirs::directory(repository, workspace_id)?,
        })
    }
}

struct RegisteredExecution {
    id: ExecutionId,
    space: PresentSpace,
    plan_space: crate::model::ExecutionSpace,
    scope_token: ScopeToken,
    ports: Vec<PortReservation>,
    environment: std::collections::BTreeMap<String, std::ffi::OsString>,
    stop: watch::Receiver<bool>,
}

/// Operations on the client type [`ExecutionKind`] that only the daemon uses.
pub(crate) trait ExecutionKindExt {
    /// Apply the kind's lifecycle checks in the registration transaction.
    fn reserve(
        self,
        tx: &Transaction<'_>,
        workspace_id: &SpaceId,
        parent_execution: Option<&ExecutionId>,
    ) -> Result<()>;
}

impl ExecutionKindExt for ExecutionKind {
    fn reserve(
        self,
        tx: &Transaction<'_>,
        workspace_id: &SpaceId,
        parent_execution: Option<&ExecutionId>,
    ) -> Result<()> {
        // Quarantine may have left a moving workspace failed, which setup
        // would otherwise accept.
        store::ensure_no_pending_move(tx, workspace_id)?;
        match self {
            Self::Command | Self::Merge | Self::Unmerge => store::require_ready(tx, workspace_id)?,
            Self::Setup => {
                let blocking: Option<ExecutionId> = tx.query_row(
                    "SELECT id FROM executions WHERE space_id=?1 AND (?2 IS NULL OR id != ?2) LIMIT 1",
                    params![workspace_id, parent_execution],
                    |r| r.get(0),
                ).optional()?;
                if let Some(blocking) = blocking {
                    bail!(
                        "workspace has active or unknown execution {blocking}; setup is unavailable until it finishes or is cleared"
                    );
                }
                let reserved = tx.execute(
                    "UPDATE workspaces SET state=?2,error=NULL,setup_finished=0,setup_skipped=0 WHERE id=?1 AND state IN (?3,?4,?2)",
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
        let request = ExecutionRequest {
            demo,
            ..Default::default()
        };
        self.begin_execution_placed(
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
    #[cfg(test)]
    pub(crate) async fn begin_agent_execution(&self, selector: &str) -> Result<StartedExecution> {
        let request = ExecutionRequest {
            agent: true,
            ..Default::default()
        };
        let selector = SpaceSelector::from(selector);
        self.begin_execution_placed(&selector, None, ExecutionKind::Command, None, request)
            .await
    }

    /// End every execution of the space `id` as exited, as their wrappers
    /// report when they exit.
    #[cfg(test)]
    pub(crate) async fn end_executions(&self, id: &SpaceId) {
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
            self.end_test_execution(execution).await;
        }
    }

    /// End execution `id` as its wrapper does once its command exits: the
    /// process group it recorded ends, then it reports the exit, which the
    /// space's log records for an agent.
    #[cfg(test)]
    pub(crate) async fn end_test_execution(&self, id: ExecutionId) {
        let query = id.clone();
        let recorded = self
            .store
            .run(move |db| store::find_execution(db, &query))
            .await
            .unwrap();
        if let Some(execution) = &recorded
            && let Some(name) = execution.agent.clone()
        {
            let ended = crate::daemon::events::AgentEvent {
                name,
                execution_id: Some(id.clone()),
                exit_code: Some(0),
            };
            let kind = crate::daemon::events::EventKind::AgentExited;
            self.record_agent(&execution.space_id, kind, ended).await;
        }
        if let Some(group) = recorded.and_then(|execution| execution.group_id) {
            // SAFETY: kill only sends a signal.
            unsafe { libc::kill(-(group as libc::pid_t), libc::SIGKILL) };
            while process::group_alive(group).await.unwrap() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
        self.finish_execution(id, ExecutionKind::Command, Some(0))
            .await
            .unwrap();
    }

    /// Register an execution and issue its scope token, recording the demo it
    /// runs and the place it runs at with it. The returned receiver fires
    /// when the workspace asks its commands to stop.
    pub(crate) async fn begin_execution_placed(
        &self,
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
        let (space_id, preparation) = self
            .prepare_execution(selector, wrapper.as_ref(), kind, caller, merge)
            .await?;
        // An agent's execution is its own, wherever the process that started
        // it runs.
        let agent_execution = match caller.filter(|_| !agent) {
            Some(caller) => self.caller_agent(caller).await.map(|agent| agent.execution),
            None => None,
        };
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
        let registered = self.register_execution(&space_id, record).await?;
        // Setup releases its Git gate before the hook; resources remain guarded
        // until pre-setup completes, while merge's gate travels with the plan.
        drop(registration_guard);
        self.run_pre_setup(&registered, kind, pre_setup.as_deref())
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
            },
            space: registered.space,
            stop: registered.stop,
            _git_guard: lifetime_guard,
        })
    }

    /// Record `record` as an execution of the space `space_id`, with what
    /// its plan reads from the space.
    async fn register_execution(
        &self,
        space_id: &SpaceId,
        record: ExecutionRecord,
    ) -> Result<RegisteredExecution> {
        let mut connections = self.executions.connections().await;
        connections.ensure_accepting()?;
        let home = self.execution_home(space_id).await?;
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
        let caches = home.caches();
        self.store
            .write(move |tx| {
                let space_id = home.id();
                home.reserve(tx, kind, agent, parent_execution.as_ref())?;
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
                        store::json_text(wrapper.as_ref())?,
                        reattach_token,
                        agent_execution
                    ],
                )?;
                if let Some(demo) = &demo {
                    super::demos::record(tx, &id, space_id, demo)?;
                }
                if let Some(place) = &place {
                    super::places::record_execution_place(tx, &id, space_id, place)?;
                }
                Ok((home, scope_token, provisions))
            })
            .await
    }

    async fn run_pre_setup(
        &self,
        registered: &RegisteredExecution,
        kind: ExecutionKind,
        pre_setup: Option<&Path>,
    ) -> Result<()> {
        // Only a workspace's setup has a pre-setup hook.
        let (Some(command), PresentSpace::Workspace(workspace)) = (pre_setup, &registered.space)
        else {
            return Ok(());
        };
        if let Err(error) = async {
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
        selector: &SpaceSelector,
        wrapper: Option<&Identity>,
        kind: ExecutionKind,
        caller: Option<&Caller>,
        merge: Option<crate::model::MergeOptions>,
    ) -> Result<(SpaceId, PreparedExecution)> {
        if let Some(wrapper) = wrapper {
            ensure!(
                process::alive(wrapper)?,
                "execution wrapper is no longer alive"
            );
        }
        let preparation = match self.space(selector).await? {
            PresentSpace::Workspace(workspace) => {
                // Its temporary directory goes in the repository's directory.
                self.reserve_repository_directory(&workspace.repository_id)
                    .await?;
                let preparation = self
                    .prepare_execution_kind(&workspace, kind, caller, merge)
                    .await?;
                (workspace.id, preparation)
            }
            // A chat runs only its agent, which its registration checks, with
            // nothing to prepare.
            PresentSpace::Chat(chat) => (chat.id, PreparedExecution::default()),
        };
        Ok(preparation)
    }

    /// `merge` says how a merge runs; a scoped `caller`'s merge or unmerge
    /// needs its permission.
    async fn prepare_execution_kind(
        &self,
        workspace: &Workspace,
        kind: ExecutionKind,
        caller: Option<&Caller>,
        merge: Option<crate::model::MergeOptions>,
    ) -> Result<PreparedExecution> {
        match kind {
            ExecutionKind::Command => Ok(PreparedExecution::default()),
            ExecutionKind::Merge => {
                let guard = self.gates.lock_git(&workspace.repository_id).await;
                let merge = self
                    .prepare_merge(
                        &SpaceSelector::id(&workspace.id),
                        &merge.unwrap_or_default(),
                    )
                    .await?;
                if let Some(caller) = caller {
                    self.authorize_scoped_merge(&merge, caller).await?;
                }
                Ok(PreparedExecution {
                    merge: Some(Box::new(merge)),
                    lifetime_guard: Some(guard),
                    ..Default::default()
                })
            }
            ExecutionKind::Unmerge => {
                let guard = self.gates.lock_git(&workspace.repository_id).await;
                let unmerge = self
                    .prepare_unmerge(&SpaceSelector::id(&workspace.id))
                    .await?;
                if let Some(caller) = caller {
                    self.authorize_scoped_unmerge(&unmerge, caller).await?;
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
                    .wait_for_creation(&workspace.repository_id, &workspace.id)
                    .await;
                let git_guard = self.gates.lock_git(&workspace.repository_id).await;
                self.ensure_no_pending_rename(&workspace.id).await?;
                let setup_cmd = self.workspace_hook(workspace, HookKind::Setup).await?;
                let pre_setup = self.workspace_hook(workspace, HookKind::PreSetup).await?;
                // A copy of a workspace's files has that workspace's ignored
                // files instead.
                let setup = &self.workspace_settings(workspace).await?.setup;
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
        self.store
            .write(move |tx| {
                let updated = tx.execute(
                    "UPDATE executions SET child=?2,group_id=?3 WHERE id=?1 AND group_id IS NULL",
                    params![id, store::json_text(child.as_ref())?, group_id],
                )?;
                ensure!(
                    updated == 1,
                    "execution already registered or no longer exists"
                );
                super::demos::mark_run(tx, &id)?;
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
    pub(crate) async fn release_exited_child(
        &self,
        id: &ExecutionId,
        space: &SpaceId,
    ) -> Result<()> {
        match self
            .pause_agent_execution(id, space, timing::PROCESS_PROOF_TIMEOUT)
            .await?
        {
            super::agents::Pause::Paused => {}
            super::agents::Pause::Unproven(issue) => {
                bail!("the previous run left processes Shoal cannot prove stopped: {issue}")
            }
        }
        self.execution_home(space).await.map(drop)
    }

    /// Close an execution. `exit_code` is `None` when the wrapper disconnected
    /// without reporting; such executions stay recorded as unknown. Returns
    /// whether the command's processes are verifiably gone. The reporting
    /// wrapper remains alive until its completion acknowledgement.
    pub async fn finish_execution(
        &self,
        id: ExecutionId,
        kind: ExecutionKind,
        exit_code: Option<i32>,
    ) -> Result<bool> {
        self.forget_proxy_reports(&id, exit_code.is_some()).await;
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
        id: ExecutionId,
        kind: ExecutionKind,
        exit_code: Option<i32>,
        scan: Option<Result<process::Scan>>,
    ) -> Result<bool> {
        let issue = self.execution_completion_issue(&id, scan).await?;
        let complete = issue.is_none();
        self.keep_stopped_turn(&id, None).await;
        let connections = self.executions.connections().await;
        let completed = self
            .record_execution_completion(&id, kind, exit_code, issue)
            .await?;
        self.ask_user_about(&completed.released).await;
        let demo = completed.demo;
        if connections.disconnect(&id).await {
            self.agent_queue.wake();
        }
        if !complete {
            self.signals.execution_became_unknown();
        }
        self.close_demo_session(demo).await;
        self.wake_runner_for_ended_job(&id).await;
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
        let (workspace_id, agent_execution, narrowing) =
            self.authorize_reattachment(request).await?;
        let space = (self.find_space(&workspace_id).await?)
            .with_context(|| format!("no space {workspace_id}"))?;
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
                space.id(),
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
            let agent = crate::execution::TrackedAgent {
                name: name.clone(),
                model: request.model.clone(),
                mode: (request.mode.clone()).unwrap_or(crate::config::modes::ModeName::DEFAULT),
                // What its launch recorded, which a reattaching wrapper
                // cannot change.
                narrowing,
            };
            self.track_agent(
                &request.execution,
                &agent,
                space.id(),
                space.name(),
                request.recover,
                Duration::from_millis(request.running_ms),
            )
            .await;
            if let Some(protection) = &request.awaiting_restore {
                self.await_agent_restore(&request.execution, protection.clone())
                    .await;
            }
        }
        self.touch(space.id()).await;
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
                let execution = store::find_execution(tx, &request.execution)?
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
    fn discard_undelivered_stop(&self, space: &PresentSpace, execution: &ExecutionId) {
        match crate::execution::recovery::discard_undelivered(&self.paths, space.id(), execution) {
            Ok(true) => log!(
                "discarded an undelivered stop for execution {execution} in {}",
                space.name()
            ),
            Ok(false) => {}
            Err(error) => {
                log!("undelivered stop for execution {execution} not discarded: {error:#}")
            }
        }
    }

    /// Drop a wrapper's connection without recording completion, so its
    /// execution awaits reattachment.
    pub(crate) async fn detach_execution(&self, id: &ExecutionId) {
        if self.executions.disconnect(id).await {
            self.agent_queue.wake();
        }
    }

    async fn execution_completion_issue(
        &self,
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
            .run(move |db| store::find_execution(db, &query_id))
            .await?;
        let Some(execution) = execution else {
            return Ok(Some("execution record is missing".into()));
        };
        Ok(match self.inspect_execution(&execution, &scan).await {
            Ok(processes) => processes.completion_issue(),
            Err(error) => Some(format!("process inspection failed: {error:#}")),
        })
    }

    /// Combine the process evidence of `execution` from `scan`.
    pub(crate) async fn inspect_execution(
        &self,
        execution: &Execution,
        scan: &process::Scan,
    ) -> Result<Processes> {
        let space = self.space(&SpaceSelector::id(&execution.space_id)).await?;
        let directory = self.space_directory(&space).await?;
        let agents = self.agent_wrappers().await?;
        Processes::inspect(execution, &directory, scan, &agents).await
    }

    /// The wrappers of the executions that run agents, each of which owns
    /// its agent wherever the process that started it runs.
    pub(crate) async fn agent_wrappers(&self) -> Result<Vec<process::Identity>> {
        self.store.read(|db| store::agent_wrappers(db)).await
    }

    /// Delete an execution record whose processes the caller proved stopped.
    pub(crate) async fn forget_execution(&self, id: &ExecutionId) -> Result<()> {
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
