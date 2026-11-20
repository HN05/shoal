//! The memory of Shoal's work: the limit `[machine] memory_limit` comes to
//! on this machine, on Linux the cap the daemon gives the work slice from
//! it, at startup and whenever the config changes it, and what the scope of
//! an ended command in the slice recorded.
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::protocol::ScopeMemory;

use super::{log, manager::Manager};
use crate::{
    config::{Config, memory_limit::MemoryLimit},
    process::load::{self, WorkLimit},
};

impl Manager {
    /// Keep what the scope of the ended execution `id` recorded of its
    /// `memory`: a CI job's runner's raises the job's peak memory and names
    /// its kills in the job's end, which a failure to record only logs.
    pub(crate) async fn record_scope_memory(&self, id: &ExecutionId, memory: ScopeMemory) {
        if let Err(error) = self.runner.record_job_memory(id, memory).await {
            log!("runner: memory of execution {id}: {error:#}");
        }
    }

    /// The user's limit on Shoal's work, with what the latest usage sample
    /// counts of it, which protection and admission count as the machine's
    /// memory; none without a limit.
    pub(crate) fn work_limit(&self) -> Option<WorkLimit> {
        let limit = limit(&self.config())?;
        let now = tokio::time::Instant::now();
        let sampled = self.usage.with_workspaces(|usages| usages.memory(now));
        Some(WorkLimit { limit, sampled })
    }
}

/// The bytes `config` limits Shoal's work to on this machine; none without
/// a limit, or for a share of RAM that cannot be read, which is logged.
pub(crate) fn limit(config: &Config) -> Option<u64> {
    let limit = config.machine.memory_limit?;
    let ram = match limit {
        MemoryLimit::Bytes(_) => 0,
        MemoryLimit::Percent(_) => (load::ram())
            .inspect_err(|error| log!("memory limit: {error:#}"))
            .ok()?,
    };
    Some(limit.bytes(ram))
}

/// Keep the work slice's cap at the configured limit, lifting it once the
/// limit is removed. A development build leaves the slice to the installed
/// Shoal, as its wrappers do.
#[cfg(target_os = "linux")]
pub(super) async fn run(manager: std::sync::Arc<Manager>) {
    use crate::service::work::{self, Cap};
    if !crate::build::changes_user_files() {
        return;
    }
    let mut config = manager.config_state.watch();
    // The cap the slice has from this daemon; a failure leaves it as it was
    // and is tried again on the next reload.
    let mut applied = None;
    loop {
        let cap = limit(&config.borrow_and_update()).map(Cap::of);
        if applied != Some(cap) {
            match (work::cap_memory(cap).await, cap) {
                (Ok(()), _) => {
                    if let Some(cap) = cap {
                        log!(
                            "work memory capped: throttled above {} bytes, killed above {}",
                            cap.high,
                            cap.max
                        );
                    }
                    applied = Some(cap);
                }
                (Err(error), Some(_)) => log!("work memory not capped: {error:#}"),
                (Err(error), None) if matches!(applied, Some(Some(_))) => {
                    log!("work memory cap not lifted: {error:#}");
                }
                // Without a limit, a slice this daemon never capped that
                // cannot be read, as without systemd, has no cap to lift.
                (Err(_), None) => {}
            }
        }
        if config.changed().await.is_err() {
            return;
        }
    }
}
