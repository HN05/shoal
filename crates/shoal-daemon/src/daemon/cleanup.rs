//! The cleanup sweep: one pass of every automatic cleanup across the
//! daemon's subsystems, every [`SWEEP_INTERVAL`] and whenever something
//! wakes it. The spaces' own cleanups, and the reports of how the sweep
//! went, are the spaces service's.
use std::sync::Arc;

use anyhow::Result;
use tokio::time::sleep;

pub use shoal_spaces::cleanup::*;

use super::{log, manager::Manager};
use crate::process::DirectoryUsers;

/// One pass of every automatic cleanup. A failed step does not skip the
/// steps after it; the error names each step that failed, in the workspace
/// it failed in where it concerns one. The spaces' stand-in runs the
/// spaces' steps in this order too, so a change to them changes its
/// `sweep`.
pub async fn sweep(manager: &Manager, timers: &mut Timers) -> Result<(), SweepFailures> {
    let spaces = &manager.spaces;
    manager.agents.cleanup_orphan_sessions(manager).await;
    let mut failed = SweepFailures::default();
    let mut step = |name: &str, result: Result<()>| failed.step(name, result);
    step(
        "ownership recheck",
        spaces.recheck_startup_failures(manager).await,
    );
    step(
        "ended executions' leases",
        manager
            .leases
            .release_leases_of_ended_executions(manager)
            .await,
    );
    step(
        "asleep agents' running leases",
        manager.leases.stop_leases_of_long_sleepers(manager).await,
    );
    step("grace periods", spaces.begin_grace_pass().await);
    step("branch observation", observe_branches(manager).await);
    step("completion cleanup", spaces.sweep_completed(manager).await);
    step("PR cleanup", manager.sweep_prs().await);
    // Every step that asks which processes use a worktree reads one listing.
    let users = DirectoryUsers::default();
    step("idle cleanup", sweep_idle(manager, timers, &users).await);
    step(
        "merged cleanup",
        merged::sweep(manager, &mut timers.merged, &users).await,
    );
    step(
        "kept branches",
        kept_branches::sweep(manager, &mut timers.kept_branches).await,
    );
    step(
        "checkpoint expiry",
        spaces.expire_checkpoints(manager).await,
    );
    step("result expiry", spaces.expire_results(manager).await);
    step("fetched copies", spaces.expire_fetched(manager).await);
    step("event expiry", manager.expire_events().await);
    step(
        "notification expiry",
        manager.notifications.expire(manager).await,
    );
    step(
        "contributions",
        manager.repositories.sweep_contributions(manager).await,
    );
    step(
        "carried commits",
        spaces.release_unclaimed_carried(manager).await,
    );
    step(
        "proposal files",
        manager.links.sweep_proposal_files(manager).await,
    );
    step(
        "temporary directories",
        spaces.sweep_tmp_dirs(manager).await,
    );
    step("release copies", manager.sweep_releases().await);
    step("leftovers", spaces.find_leftovers(manager).await);
    step("grace periods", spaces.end_unclaimed_grace().await);
    failed.into_result()
}

/// Sweep every [`SWEEP_INTERVAL`], or at once when something wakes the
/// sweep, logging each failure once while it repeats.
pub async fn run(manager: Arc<Manager>) {
    let mut timers = Timers::default();
    let mut logged = LoggedFailures::default();
    loop {
        {
            let _operation = manager.gates.background_pass().await;
            let result = sweep(&manager, &mut timers).await;
            let failed = logged.new_failures(&result);
            if let Some(failed) = &failed {
                log!("auto cleanup: {failed}");
            }
            manager.spaces.record_sweep(&*manager, result).await;
            manager.spaces.record_cleanup_run(failed).await;
        }
        tokio::select! {
            _ = sleep(SWEEP_INTERVAL) => {},
            _ = manager.signals.cleanup_woken() => {},
        }
    }
}
