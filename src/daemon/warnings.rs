//! Message running agents when machine load nears a protection's stop
//! threshold, so they can reduce it before Shoal stops or cleans anything.
use std::{collections::HashMap, time::Duration};
use tokio::time::Instant;

use super::workspace::Manager;
use crate::{config::overload::Overload, daemon::log};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Signal {
    Memory,
    Cpu,
    Disk,
}

/// When each workspace was last warned about each signal.
#[derive(Default)]
pub(super) struct Warnings {
    sent: HashMap<(String, Signal), Instant>,
}

impl Warnings {
    /// Queue `message` for each workspace with a running agent that has not
    /// been warned about `signal` within the repeat interval, including agents
    /// started since the last warning.
    pub(super) async fn warn(
        &mut self,
        manager: &Manager,
        now: Instant,
        signal: Signal,
        message: &str,
    ) {
        let repeat = Duration::from_secs(manager.config().overload.warning.repeat_minutes * 60);
        self.sent
            .retain(|_, sent| now.duration_since(*sent) < repeat);
        for workspace_id in manager.running_agent_workspaces().await {
            let key = (workspace_id, signal);
            if self.sent.contains_key(&key) {
                continue;
            }
            if let Err(error) = manager.queue_agent_message(&key.0, message.into()).await {
                log!("pressure warning not queued: {error:#}");
            }
            self.sent.insert(key, now);
        }
    }
}

const REDUCE_LOAD: &str = "Avoid starting builds, tests or servers you do not need now, and stop processes you no longer need.";

pub(super) fn memory(settings: &Overload) -> String {
    let level = if cfg!(target_os = "macos") {
        "macOS reports memory pressure; Shoal stops an agent at critical pressure.".to_owned()
    } else {
        format!(
            "Memory use is above {}%; Shoal stops an agent at {}%.",
            settings.warning.memory_used_percent, settings.memory.used_percent
        )
    };
    format!("{level} {REDUCE_LOAD}")
}

pub(super) fn cpu(settings: &Overload) -> String {
    format!(
        "CPU use has been above {}% for {} seconds; Shoal stops an agent after {} seconds above {}%. Run builds and tests with fewer parallel jobs. {REDUCE_LOAD}",
        settings.warning.cpu_used_percent,
        settings.warning.cpu_sustained_seconds,
        settings.cpu.sustained_seconds,
        settings.cpu.used_percent,
    )
}

/// `available` describes the filesystem with the least free space.
pub(super) fn disk(settings: &Overload, available: &str) -> String {
    format!(
        "Free disk space is low: {available}; Shoal stops running agents and tracked commands below {} GiB. Delete build output and caches you no longer need, and avoid large downloads.",
        settings.disk.stop_free_gib,
    )
}

#[cfg(test)]
mod tests;
