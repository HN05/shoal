//! Reconcile stopped agents whose executions became unknown, after a daemon
//! interruption or a stop that could not be proven. A saved recovery record
//! names the stop Shoal started; once its wrapper is gone and no process of
//! the execution survives, the record alone represents the agent and
//! `shoal restore` restores it. Survivors and incomplete evidence keep the
//! execution, are checked again, and only they notify the user. Executions
//! of an earlier boot need no evidence, and those that never started their
//! command need only their wrapper gone.
use crate::client::execution_ids::ExecutionId;
use crate::execution::recovery::RecordExt;
use std::{collections::HashSet, sync::Arc, time::Duration};

use anyhow::Result;

use super::{notifications::NotificationKind, store, workspace::Manager};
use crate::daemon::log;
use crate::{
    execution::recovery::{Record, Stage, record_path},
    model::{Execution, PresentSpace},
    process::{execution::Processes, identity as process},
    state::ExecutionState,
};

/// How often to look again at wrappers that were alive during a pass.
const WRAPPER_POLL_INTERVAL: Duration = Duration::from_secs(1);
/// How long to wait before repeating a pass that failed.
const RETRY_INTERVAL: Duration = Duration::from_secs(30);
/// How often to look again at executions whose processes survive.
const UNRESOLVED_RECHECK_INTERVAL: Duration = Duration::from_secs(30);

mod earlier_boot;
mod never_started;

use never_started::Unstarted;

/// An unknown execution of an agent whose stop saved a recovery record.
struct Interrupted {
    space: PresentSpace,
    execution: Execution,
    record: Record,
}

/// The unknown executions of present spaces, by what reconciles them.
#[derive(Default)]
struct Unknown {
    interrupted: Vec<Interrupted>,
    unstarted: Vec<Unstarted>,
    unreadable: Vec<Unreadable>,
}

/// An unknown execution whose saved recovery record cannot be read.
struct Unreadable {
    space: PresentSpace,
    execution: ExecutionId,
    error: anyhow::Error,
}

/// A wrapper that was alive during reconciliation, so it may still reattach
/// or report its exit.
pub(crate) struct Waiting {
    execution: ExecutionId,
    wrapper: process::Identity,
}

/// What a pass left for a later one.
#[derive(Default)]
pub(crate) struct Pending {
    pub waiting: Vec<Waiting>,
    /// Some execution kept surviving processes or incomplete evidence.
    pub unresolved: bool,
}

/// What reconciliation concluded for one interrupted agent.
#[derive(Debug, PartialEq)]
enum Outcome {
    Waiting,
    /// No process of the execution remains.
    Stopped,
    /// Processes survive or the evidence cannot prove that none do.
    Unresolved(String),
}

/// Reconcile at startup and whenever an execution becomes unknown or a pass
/// may conclude differently, repeating a pass that failed until one succeeds.
pub(super) async fn run(manager: Arc<Manager>) {
    reconcile(&manager, UNRESOLVED_RECHECK_INTERVAL).await
}

/// `run`, looking again at unresolved executions after `recheck`.
pub(super) async fn reconcile(manager: &Manager, recheck: Duration) {
    loop {
        let pending = reconcile_until_done(manager).await;
        tokio::select! {
            () = manager.signals.execution_unknown() => {}
            () = await_change(manager, pending, recheck) => {}
        }
    }
}

/// Resolves once a waiting wrapper exits or unresolved executions are due for
/// another look.
async fn await_change(manager: &Manager, pending: Pending, recheck: Duration) {
    let Pending {
        mut waiting,
        unresolved,
    } = pending;
    let recheck = async {
        if !unresolved {
            std::future::pending::<()>().await;
        }
        tokio::time::sleep(recheck).await;
    };
    let exited = async {
        while !waiting.is_empty() {
            tokio::time::sleep(WRAPPER_POLL_INTERVAL).await;
            waiting = still_unknown(manager, waiting).await;
            if waiting
                .iter()
                .any(|entry| !process::alive(&entry.wrapper).unwrap_or(true))
            {
                return;
            }
        }
        std::future::pending::<()>().await;
    };
    tokio::select! {
        () = recheck => {}
        () = exited => {}
    }
}

async fn reconcile_until_done(manager: &Manager) -> Pending {
    loop {
        let result = {
            let _operation = manager.gates.operation().await;
            manager.reconcile_interrupted_agents().await
        };
        match result {
            Ok(pending) => return pending,
            Err(error) => log!(
                "stopped agent reconciliation failed; retrying in {}s: {error:#}",
                RETRY_INTERVAL.as_secs()
            ),
        }
        tokio::time::sleep(RETRY_INTERVAL).await;
    }
}

/// A reattached wrapper reports to the daemon like any other. An entry whose
/// state cannot be read stays, so the next pass decides it.
async fn still_unknown(manager: &Manager, waiting: Vec<Waiting>) -> Vec<Waiting> {
    let mut unknown = Vec::with_capacity(waiting.len());
    for entry in waiting {
        match manager.execution_unknown(&entry.execution).await {
            Ok(false) => {}
            Ok(true) => unknown.push(entry),
            Err(error) => {
                log!("stopped agent reconciliation: {error:#}");
                unknown.push(entry);
            }
        }
    }
    unknown
}

impl Manager {
    /// Forget unknown executions of an earlier boot, of stopped agents
    /// whose processes are gone, keeping their recovery records, and of
    /// commands that never started, and notify about the stopped agents that
    /// remain uncertain.
    pub(crate) async fn reconcile_interrupted_agents(&self) -> Result<Pending> {
        self.forget_earlier_boot().await?;
        let Unknown {
            interrupted,
            unstarted,
            unreadable,
        } = self.unknown_executions().await?;
        for unreadable in &unreadable {
            self.notify_unreadable(unreadable).await;
        }
        let mut pending = Pending::default();
        let ids: HashSet<_> = (interrupted.iter().map(|i| i.execution.id.clone()))
            .chain(unstarted.iter().map(Unstarted::id))
            .collect();
        if ids.is_empty() {
            return Ok(pending);
        }
        let scan = process::scan(ids).await?;
        for interrupted in interrupted {
            self.reconcile_interrupted(interrupted, &scan, &mut pending)
                .await?;
        }
        for unstarted in unstarted {
            self.reconcile_unstarted(unstarted, &scan, &mut pending)
                .await?;
        }
        Ok(pending)
    }

    /// Forget the stopped agent of `interrupted` once `scan` and its process
    /// identities prove it stopped, adding to `pending` what a later pass
    /// decides.
    async fn reconcile_interrupted(
        &self,
        interrupted: Interrupted,
        scan: &process::Scan,
        pending: &mut Pending,
    ) -> Result<()> {
        let inspected =
            (self.inspect_unknown(&interrupted.space, &interrupted.execution, scan)).await;
        let outcome = match inspected {
            Ok(processes) => assess(&interrupted, &processes),
            Err(error) => Outcome::Unresolved(format!("process inspection failed: {error:#}")),
        };
        match outcome {
            Outcome::Waiting => pending.waiting.extend(interrupted.waiting()),
            Outcome::Stopped => self.forget_stopped_agent(&interrupted).await?,
            Outcome::Unresolved(issue) => {
                pending.unresolved = true;
                self.notify_unresolved(&interrupted, &issue).await;
            }
        }
        Ok(())
    }

    /// The processes of `execution` in `space`, from `scan`.
    async fn inspect_unknown(
        &self,
        space: &PresentSpace,
        execution: &Execution,
        scan: &process::Scan,
    ) -> Result<Processes> {
        let directory = self.space_directory(space).await?;
        let agents = self.agent_wrappers().await?;
        Processes::inspect(execution, &directory, scan, &agents).await
    }

    /// The unknown executions of present spaces: those with a saved
    /// recovery record, those that never started a command, which have
    /// neither a record nor a recorded child or process group, and those
    /// whose record cannot be read.
    async fn unknown_executions(&self) -> Result<Unknown> {
        let mut unknown = Unknown::default();
        for space in self.present_spaces().await? {
            let id = space.id().clone();
            let executions = self.store.run(move |db| store::executions(db, &id)).await?;
            for execution in executions {
                if execution.state != ExecutionState::Unknown {
                    continue;
                }
                let path = record_path(&self.paths, space.id(), &execution.id);
                match Record::load(&path) {
                    Ok(Some(record)) => unknown.interrupted.push(Interrupted {
                        space: space.clone(),
                        execution,
                        record,
                    }),
                    Ok(None) => unknown
                        .unstarted
                        .extend(Unstarted::of(space.clone(), execution)),
                    Err(error) => unknown.unreadable.push(Unreadable {
                        space: space.clone(),
                        execution: execution.id,
                        error,
                    }),
                }
            }
        }
        Ok(unknown)
    }

    async fn forget_stopped_agent(&self, interrupted: &Interrupted) -> Result<()> {
        self.keep_stopped_turn(&interrupted.execution.id, None)
            .await;
        let id = interrupted.execution.id.clone();
        let forgotten = self
            .store
            .run(move |db| {
                Ok(db.execute(
                    "DELETE FROM executions WHERE id=?1 AND state=?2",
                    rusqlite::params![id, ExecutionState::Unknown],
                )? == 1)
            })
            .await?;
        if forgotten {
            log!(
                "reconciled stopped {} in {}",
                interrupted.record.agent,
                interrupted.space.name()
            );
        }
        Ok(())
    }

    async fn notify_unresolved(&self, interrupted: &Interrupted, issue: &str) {
        let Interrupted {
            space,
            execution,
            record,
        } = interrupted;
        let reason = record
            .stop_reason
            .as_deref()
            .map_or(String::new(), |reason| format!(" ({reason})"));
        self.notify_space(space.name(), space.id(),
            NotificationKind::RecoveryBlocked,
            format!(
                "{} stopped{reason}, but its execution {} is not proven stopped: {issue}; Shoal reconciles it once those processes exit, or stop them with shoal stop {}; then run shoal restore {} --execution {}",
                record.agent, execution.id, space.number(), space.number(), execution.id
            ),
        )
        .await;
    }

    async fn notify_unreadable(&self, unreadable: &Unreadable) {
        let Unreadable {
            space,
            execution,
            error,
        } = unreadable;
        self.notify_space(space.name(), space.id(),
            NotificationKind::RecoveryBlocked,
            format!(
                "The recovery record of execution {execution} cannot be read: {error:#}; check or remove {}",
                crate::execution::recovery::record_path(&self.paths, space.id(), execution).display()
            ),
        )
        .await;
    }

    async fn execution_unknown(&self, id: &ExecutionId) -> Result<bool> {
        let id = id.to_owned();
        let execution = self
            .store
            .run(move |db| store::find_execution(db, &id))
            .await?;
        Ok(execution.is_some_and(|execution| execution.state == ExecutionState::Unknown))
    }
}

impl Interrupted {
    fn waiting(&self) -> Option<Waiting> {
        Some(Waiting {
            execution: self.execution.id.clone(),
            wrapper: self.execution.wrapper.clone()?,
        })
    }
}

fn assess(interrupted: &Interrupted, processes: &Processes) -> Outcome {
    if processes.wrapper.is_some() {
        return Outcome::Waiting;
    }
    if let Some(issue) = processes.completion_issue() {
        return Outcome::Unresolved(issue);
    }
    // The daemon forgets a stopped agent's child only after its wrapper
    // reports the stop and the process inventory confirms it.
    let paused =
        interrupted.record.stage == Some(Stage::Stopped) && interrupted.execution.wrapper.is_some();
    if processes.launch_recorded || paused {
        Outcome::Stopped
    } else {
        Outcome::Unresolved("its process identity is incomplete".into())
    }
}
