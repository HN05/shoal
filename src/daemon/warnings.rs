//! Message running agents when machine load nears a protection's stop
//! threshold, so they can reduce it before Shoal stops or cleans anything.
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};
use tokio::time::Instant;

use super::{agent_messages::Delivery, workspace::Manager};
use crate::{config::overload::Overload, daemon::log};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Signal {
    Memory,
    Cpu,
    Disk,
}

impl Signal {
    /// Whether an agent reduces it only through what runs in its workspace,
    /// unlike disk space, which it can free by deleting files.
    fn needs_processes(self) -> bool {
        matches!(self, Self::Memory | Self::Cpu)
    }
}

/// When each workspace was last warned about each signal.
#[derive(Default)]
pub(super) struct Warnings {
    sent: HashMap<(String, Signal), Instant>,
}

impl Warnings {
    /// Queue `message` for each workspace with a running agent that has not
    /// been warned about `signal` within the repeat interval, including agents
    /// started since the last warning. An idle agent with nothing running has
    /// nothing to stop for a signal that needs processes, so the message
    /// waits for its next turn instead of starting one.
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
        let resting = if signal.needs_processes() {
            resting_workspaces(manager).await
        } else {
            HashSet::new()
        };
        for workspace_id in manager.running_agent_workspaces().await {
            let key = (workspace_id, signal);
            if self.sent.contains_key(&key) {
                continue;
            }
            let delivery = if resting.contains(&key.0) {
                Delivery::Quiet
            } else {
                Delivery::Wake
            };
            if let Err(error) = manager
                .queue_agent_message_delivered(&key.0, message.into(), delivery)
                .await
            {
                log!("pressure warning not queued: {error:#}");
            }
            self.sent.insert(key, now);
        }
    }
}

/// [`Manager::resting_agent_workspaces`], or none when they cannot be read,
/// so every agent is warned.
async fn resting_workspaces(manager: &Manager) -> HashSet<String> {
    manager
        .resting_agent_workspaces()
        .await
        .unwrap_or_else(|error| {
            log!("pressure warning wakes every agent: {error:#}");
            HashSet::new()
        })
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
