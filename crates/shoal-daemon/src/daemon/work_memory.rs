//! The memory of Shoal's work: the limit `[machine] memory_limit` comes to
//! on this machine, on Linux the cap the daemon gives the work slice from
//! it, at startup and whenever the config changes it, and what the scope of
//! an ended command in the slice recorded.
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::protocol::{ScopeMemory, WorkMemoryLimit};

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
        let sampled = self
            .spaces
            .usage
            .with_workspaces(|usages| usages.memory(now));
        Some(WorkLimit { limit, sampled })
    }
}

/// Whether work that cannot be read holds back starts and recovery under
/// `config`: with a limit, which memory protection reads.
fn unread_work_holds(config: &Config) -> bool {
    config.machine.overload.memory.enabled && config.machine.work_memory_limit().limit.is_some()
}

/// Tells the user once, while containers the usage samples cannot read hold
/// back work under the limit, and again after they have been read.
#[derive(Default)]
pub(crate) struct UnreadContainers {
    warned: bool,
}

impl UnreadContainers {
    /// Follow a sample that could not read the containers `missed` names,
    /// or read them all without it, telling the user when unread work
    /// begins to hold back starts.
    pub(crate) async fn observe(&mut self, manager: &Manager, missed: Option<String>) {
        let holding = missed.filter(|_| unread_work_holds(&manager.config()));
        if !self.begins(holding.is_some()) {
            return;
        }
        let message = format!(
            "Shoal's work memory cannot be read under memory_limit, so no agent starts and no stopped agent recovers, until every running container project can be read: {}",
            holding.unwrap_or_default()
        );
        let kind = crate::daemon::alerts::AlertKind::LoadWarning;
        manager
            .coordination
            .notify(manager, None, kind, message)
            .await;
    }

    /// Whether unread work that now holds back starts, `holding`, began to
    /// with this sample.
    fn begins(&mut self, holding: bool) -> bool {
        let began = holding && !self.warned;
        self.warned = holding;
        began
    }
}

/// The bytes `config` limits Shoal's work to on this machine; none without
/// a limit, or for a share of RAM that cannot be read, which is logged.
pub(crate) fn limit(config: &Config) -> Option<u64> {
    in_bytes(config.machine.work_memory_limit().limit?)
}

/// The limit in effect under `config`, as `shoal status` shows it.
pub(crate) fn limit_status(config: &Config) -> WorkMemoryLimit {
    let effect = config.machine.work_memory_limit();
    WorkMemoryLimit {
        bytes: effect.limit.and_then(in_bytes),
        percent: match effect.limit {
            Some(MemoryLimit::Percent(percent)) => Some(percent),
            _ => None,
        },
        source: effect.source,
    }
}

/// `limit` in bytes on this machine; none for a share of RAM that cannot be
/// read, which is logged.
fn in_bytes(limit: MemoryLimit) -> Option<u64> {
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
    if !shoal_core::build::changes_user_files() {
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

#[cfg(test)]
mod tests;
