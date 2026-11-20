//! The memory limit of Shoal's work, `[machine] memory_limit`: what it
//! comes to on this machine, and on Linux the cap the daemon gives the work
//! slice from it, at startup and whenever the config changes it.
use super::log;
use crate::{
    config::{Config, memory_limit::MemoryLimit},
    process::load,
};

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
pub(super) async fn run(manager: std::sync::Arc<super::manager::Manager>) {
    use crate::service::work::{self, Cap};
    if !crate::build::changes_user_files() {
        return;
    }
    let mut config = manager.config_state.watch();
    // Tried once per limit; a failure waits for the next change.
    let mut tried = None;
    loop {
        let cap = limit(&config.borrow_and_update()).map(Cap::of);
        if tried != Some(cap) {
            tried = Some(cap);
            match (work::cap_memory(cap).await, cap) {
                (Ok(()), Some(cap)) => log!(
                    "work memory capped: throttled above {} bytes, killed above {}",
                    cap.high,
                    cap.max
                ),
                (Err(error), Some(_)) => log!("work memory not capped: {error:#}"),
                // Without a limit, a slice that cannot be read has no cap
                // Shoal gave it.
                (_, None) => {}
            }
        }
        if config.changed().await.is_err() {
            return;
        }
    }
}
