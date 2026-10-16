//! Reconcile stopped agents that a daemon interruption left quarantined. A
//! saved recovery record names the stop Shoal started; once its wrapper is gone
//! and no process of the execution survives, the record alone represents the
//! agent and `shoal resume` restores it. Survivors and incomplete evidence keep
//! the execution for `shoal doctor`, and only they notify the user.
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

/// How often to look again at wrappers that were alive at startup.
const WRAPPER_POLL_INTERVAL: Duration = Duration::from_secs(1);
/// How long to wait before repeating a pass that failed.
const RETRY_INTERVAL: Duration = Duration::from_secs(30);

/// An execution quarantined by a restart whose stop saved a recovery record.
struct Interrupted {
    workspace: Workspace,
    execution: Execution,
    record: Record,
}

/// A wrapper that was alive during reconciliation, so it may still reattach
/// or report its exit.
pub(crate) struct Waiting {
    execution: String,
    wrapper: process::Identity,
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

/// Reconcile at startup, then again whenever a wrapper that was alive exits,
/// repeating a pass that failed until one succeeds.
pub(super) async fn run(manager: Arc<Manager>) {
    let mut waiting = reconcile_until_done(&manager).await;
    while !waiting.is_empty() {
        tokio::time::sleep(WRAPPER_POLL_INTERVAL).await;
        waiting = still_unknown(&manager, waiting).await;
        if waiting
            .iter()
            .any(|entry| !process::alive(&entry.wrapper).unwrap_or(true))
        {
            waiting = reconcile_until_done(&manager).await;
        }
    }
}

async fn reconcile_until_done(manager: &Manager) -> Vec<Waiting> {
    loop {
        let result = {
            let _operation = manager.background_operations.read().await;
            manager.reconcile_interrupted_agents().await
        };
        match result {
            Ok(waiting) => return waiting,
            Err(error) => log!(
                "restart reconciliation failed; retrying in {}s: {error:#}",
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
                log!("restart reconciliation: {error:#}");
                unknown.push(entry);
            }
        }
    }
    unknown
}

impl Manager {
    /// Forget quarantined executions of stopped agents whose processes are
    /// gone, keeping their recovery records, and notify about the ones that
    /// remain uncertain. Returns the wrappers that are still alive.
    pub(crate) async fn reconcile_interrupted_agents(&self) -> Result<Vec<Waiting>> {
        let interrupted = self.interrupted_agents().await?;
        if interrupted.is_empty() {
            return Ok(vec![]);
        }
        let ids: HashSet<_> = interrupted.iter().map(|i| i.execution.id.clone()).collect();
        let scan = process::scan(ids).await?;
        let mut waiting = Vec::new();
        for interrupted in interrupted {
            let outcome = match Processes::inspect(&interrupted.execution, &scan).await {
                Ok(processes) => assess(&interrupted, &processes),
                Err(error) => Outcome::Unresolved(format!("process inspection failed: {error:#}")),
            };
            match outcome {
                Outcome::Waiting => waiting.extend(interrupted.waiting()),
                Outcome::Stopped => self.forget_stopped_agent(&interrupted).await?,
                Outcome::Unresolved(issue) => self.notify_unresolved(&interrupted, &issue).await,
            }
        }
        Ok(waiting)
    }

    async fn interrupted_agents(&self) -> Result<Vec<Interrupted>> {
        let mut interrupted = Vec::new();
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
                    // Doctor still reports the execution; resume reports the record.
                    Err(error) => log!("restart reconciliation skipped a record: {error:#}"),
                }
            }
        }
        Ok(interrupted)
    }

    async fn forget_stopped_agent(&self, interrupted: &Interrupted) -> Result<()> {
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
                "reconciled stopped {} in {} after the daemon restarted",
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
            .map_or(String::new(), |reason| format!(" after {reason}"));
        self.notify(
            Some(&workspace.name),
            NotificationKind::RecoveryBlocked,
            format!(
                "{} stopped{reason}, but its execution {} cannot be reconciled after the daemon restarted: {issue}; inspect it with shoal doctor {}, then run shoal resume {} --execution {}",
                record.agent, execution.id, workspace.name, workspace.name, execution.id
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
