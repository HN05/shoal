//! Running repositories' CI jobs on this machine: the runner each
//! repository registers on its forge, the forge's waiting jobs as entries of
//! the agent queue, and the jobs the daemon claims, each in a workspace at its
//! commit that goes when the job ends.
//!
//! The runner's background work: each repository's runner follows its
//! `runner.enabled` setting, checked when the global config or a local
//! config changes and every [`INTERVAL`] for the repositories' own files; every
//! [`POLL_INTERVAL`], and whenever admission admits a job, the jobs that
//! ended give up their workspaces, admitted jobs are claimed, and the
//! forge's waiting jobs are read while one could start, and every
//! [`INTERVAL`] while none can.
mod claims;
mod costs;
mod ends;
mod linked;
mod listing;
mod overrun;
mod registration;
mod single_use;
mod superseded;
mod taken;
mod waiting;

pub(crate) use registration::Registration;
pub(crate) use shoal_runner::jobs::JobStop;

use std::{sync::Arc, time::Duration};

use shoal_runner::jobs::INTERVAL;
use tokio::time::Instant;

use crate::daemon::{log, manager::Manager};

/// How often the forge's waiting jobs are read while one could start.
const POLL_INTERVAL: Duration = Duration::from_secs(10);

pub(super) async fn run(manager: Arc<Manager>) {
    let mut config = manager.config_state.watch();
    let mut reconciled: Option<Instant> = None;
    loop {
        {
            // A daemon update waits for the pass, as for any background
            // operation, and an exit that waits holds it back.
            let _operation = manager.gates.background_pass().await;
            let changed = manager.runner.jobs().take_changed();
            if changed {
                // A changed setting deserves a try at once.
                manager.runner.jobs().forget_failures();
            }
            if changed || reconciled.is_none_or(|at| at.elapsed() >= INTERVAL) {
                if let Err(error) = manager.reconcile_runners().await {
                    log!("runner: {error:#}");
                }
                reconciled = Some(Instant::now());
            }
            if let Err(error) = pass(&manager).await {
                log!("runner: {error:#}");
            }
        }
        tokio::select! {
            _ = config.changed() => {
                manager.runner.jobs().forget_failures();
                reconciled = None;
            }
            () = manager.runner.jobs().woken() => {}
            _ = tokio::time::sleep(POLL_INTERVAL) => {}
        }
    }
}

/// Record the running jobs' usage, remove the workspaces of the jobs that
/// ended, move a job's workspace to the commit of the job its runner took
/// instead, release the runners the forge gave no job, claim the jobs
/// admission admitted, and read the forge's waiting jobs when due.
async fn pass(manager: &Arc<Manager>) -> anyhow::Result<()> {
    manager.sample_job_usage().await?;
    manager.finish_ended_jobs().await?;
    manager.follow_taken_jobs().await?;
    manager.release_idle_runners().await?;
    claim_admitted_jobs(manager).await?;
    manager.poll_waiting_jobs().await
}

/// Claim each admitted job no claim runs for yet, each in a task of its own
/// that a daemon update waits for, and wake admission once it ends.
async fn claim_admitted_jobs(manager: &Arc<Manager>) -> anyhow::Result<()> {
    for entry in manager.admitted_jobs().await? {
        if !manager.runner.jobs().begin_claim(&entry.id) {
            continue;
        }
        let manager = manager.clone();
        let operation = manager.gates.owned_operation().await;
        tokio::spawn(async move {
            let _operation = operation;
            manager.claim_job(&entry).await;
            manager.runner.jobs().end_claim(&entry.id);
            manager.agent_queue.wake();
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests;
