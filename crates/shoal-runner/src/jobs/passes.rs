//! The runner's background work: each repository's runner follows its
//! `runner.enabled` setting, checked when the global config or a local
//! config changes and every [`INTERVAL`] for the repositories' own files;
//! every [`POLL_INTERVAL`], and whenever admission admits a job, the jobs
//! that ended give up their workspaces, admitted jobs are claimed, and the
//! forge's waiting jobs are read while one could start, and every
//! [`INTERVAL`] while none can.
use std::{sync::Arc, time::Duration};

use anyhow::Result;
use tokio::time::Instant;

use super::INTERVAL;
use crate::{Runner, daemon::Daemon, log};

/// How often the forge's waiting jobs are read while one could start.
const POLL_INTERVAL: Duration = Duration::from_secs(10);

/// Run the runner's passes until the daemon stops.
pub async fn run(runner: Arc<Runner>, daemon: Arc<dyn Daemon>) {
    let mut config = runner.config.watch();
    let mut reconciled: Option<Instant> = None;
    loop {
        {
            // A daemon update waits for the pass, as for any background
            // operation, and an exit that waits holds it back.
            let _operation = runner.gates.background_pass().await;
            let changed = runner.jobs.take_changed();
            if changed {
                // A changed setting deserves a try at once.
                runner.jobs.forget_failures();
            }
            if changed || reconciled.is_none_or(|at| at.elapsed() >= INTERVAL) {
                if let Err(error) = runner.reconcile_runners(&*daemon).await {
                    log!("runner: {error:#}");
                }
                reconciled = Some(Instant::now());
            }
            if let Err(error) = pass(&runner, &daemon).await {
                log!("runner: {error:#}");
            }
        }
        tokio::select! {
            _ = config.changed() => {
                runner.jobs.forget_failures();
                reconciled = None;
            }
            () = runner.jobs.woken() => {}
            _ = tokio::time::sleep(POLL_INTERVAL) => {}
        }
    }
}

/// Record the running jobs' usage, remove the workspaces of the jobs that
/// ended, rerun those protection stopped whose runs have finished, move a
/// job's workspace to the commit of the job its runner took instead,
/// release the runners the forge gave no job, claim the jobs admission
/// admitted, and read the forge's waiting jobs when due.
async fn pass(runner: &Arc<Runner>, daemon: &Arc<dyn Daemon>) -> Result<()> {
    runner.sample_job_usage(&**daemon).await?;
    runner.finish_ended_jobs(&**daemon).await?;
    runner.rerun_stopped_jobs(&**daemon).await?;
    runner.follow_taken_jobs(&**daemon).await?;
    runner.release_idle_runners(&**daemon).await?;
    claim_admitted_jobs(runner, daemon).await?;
    runner.poll_waiting_jobs(&**daemon).await
}

/// Claim each admitted job no claim runs for yet, each in a task of its own
/// that a daemon update waits for, and wake admission once it ends.
async fn claim_admitted_jobs(runner: &Arc<Runner>, daemon: &Arc<dyn Daemon>) -> Result<()> {
    for entry in runner.admitted_jobs(&**daemon).await? {
        if !runner.jobs.begin_claim(&entry.id) {
            continue;
        }
        let (runner, daemon) = (runner.clone(), daemon.clone());
        let operation = runner.gates.owned_operation().await;
        tokio::spawn(async move {
            let _operation = operation;
            runner.claim_job(&*daemon, &entry).await;
            runner.jobs.end_claim(&entry.id);
            daemon.wake_admission();
        });
    }
    Ok(())
}
