//! Reconcile stopped agents whose executions became unknown, after a daemon
//! interruption or a stop that could not be proven. A saved recovery record
//! names the stop Shoal started; once its wrapper is gone and no process of
//! the execution survives, the record alone represents the agent and
//! `shoal resume` restores it. Survivors and incomplete evidence keep the
//! execution, are checked again, and only they notify the user. Executions
//! of an earlier boot need no evidence.
use std::{collections::HashSet, sync::Arc, time::Duration};

use anyhow::Result;

use super::{notifications::NotificationKind, store, workspace::Manager};
use crate::daemon::log;
use crate::{
    execution::recovery::{Record, Stage, record_path},
    model::{Execution, Workspace},
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

/// An unknown execution of an agent whose stop saved a recovery record.
struct Interrupted {
    workspace: Workspace,
    execution: Execution,
    record: Record,
}

/// An unknown execution whose saved recovery record cannot be read.
struct Unreadable {
    workspace: Workspace,
    execution: String,
    error: anyhow::Error,
}

/// A wrapper that was alive during reconciliation, so it may still reattach
/// or report its exit.
pub(crate) struct Waiting {
    execution: String,
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
    /// Forget unknown executions of an earlier boot and of stopped agents
    /// whose processes are gone, keeping their recovery records, and notify
    /// about the ones that remain uncertain.
    pub(crate) async fn reconcile_interrupted_agents(&self) -> Result<Pending> {
        self.forget_earlier_boot().await?;
        let (interrupted, unreadable) = self.interrupted_agents().await?;
        for unreadable in &unreadable {
            self.notify_unreadable(unreadable).await;
        }
        let mut pending = Pending::default();
        if interrupted.is_empty() {
            return Ok(pending);
        }
        let ids: HashSet<_> = interrupted.iter().map(|i| i.execution.id.clone()).collect();
        let scan = process::scan(ids).await?;
        for interrupted in interrupted {
            let outcome = match Processes::inspect(
                &interrupted.execution,
                &interrupted.workspace.path,
                &scan,
            )
            .await
            {
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
        }
        Ok(pending)
    }

    async fn interrupted_agents(&self) -> Result<(Vec<Interrupted>, Vec<Unreadable>)> {
        let (mut interrupted, mut unreadable) = (Vec::new(), Vec::new());
        for workspace in self.list_workspaces().await? {
            let id = workspace.id.clone();
            let executions = self.store.run(move |db| store::executions(db, &id)).await?;
            for execution in executions {
                if execution.state != ExecutionState::Unknown {
                    continue;
                }
                let path = record_path(&self.paths, &workspace.id, &execution.id);
                match Record::load(&path) {
                    Ok(Some(record)) => interrupted.push(Interrupted {
                        workspace: workspace.clone(),
                        execution,
                        record,
                    }),
                    Ok(None) => {}
                    Err(error) => unreadable.push(Unreadable {
                        workspace: workspace.clone(),
                        execution: execution.id,
                        error,
                    }),
                }
            }
        }
        Ok((interrupted, unreadable))
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
                interrupted.workspace.name
            );
        }
        Ok(())
    }

    async fn notify_unresolved(&self, interrupted: &Interrupted, issue: &str) {
        let Interrupted {
            workspace,
            execution,
            record,
        } = interrupted;
        let reason = record
            .stop_reason
            .as_deref()
            .map_or(String::new(), |reason| format!(" ({reason})"));
        self.notify(
            Some(&workspace.name),
            NotificationKind::RecoveryBlocked,
            format!(
                "{} stopped{reason}, but its execution {} is not proven stopped: {issue}; Shoal reconciles it once those processes exit, or stop them with shoal stop {}; then run shoal resume {} --execution {}",
                record.agent, execution.id, workspace.name, workspace.name, execution.id
            ),
        )
        .await;
    }

    async fn notify_unreadable(&self, unreadable: &Unreadable) {
        let Unreadable {
            workspace,
            execution,
            error,
        } = unreadable;
        self.notify(
            Some(&workspace.name),
            NotificationKind::RecoveryBlocked,
            format!(
                "The recovery record of execution {execution} cannot be read: {error:#}; check or remove {}",
                crate::execution::recovery::record_path(&self.paths, &workspace.id, execution).display()
            ),
        )
        .await;
    }

    async fn execution_unknown(&self, id: &str) -> Result<bool> {
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
