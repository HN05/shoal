//! Message running agents when machine load nears a protection's stop
//! threshold, so they know of it before Shoal stops or cleans anything.
use crate::client::space_ids::SpaceId;
use std::{collections::HashMap, time::Duration};
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
    /// How its warning reaches an idle agent: a memory or CPU warning is a
    /// heads-up that asks for nothing, so it waits for the agent's next turn,
    /// while a disk warning wakes it to delete files.
    fn delivery(self) -> Delivery {
        match self {
            Self::Memory | Self::Cpu => Delivery::NextTurn,
            Self::Disk => Delivery::Wake,
        }
    }
}

/// When each workspace was last warned about each signal.
#[derive(Default)]
pub(super) struct Warnings {
    sent: HashMap<(SpaceId, Signal), Instant>,
}

impl Warnings {
    /// Queue `message` for each workspace with a running agent that has not
    /// been warned about `signal` within the repeat interval, including agents
    /// started since the last warning, delivered as [`Signal::delivery`] says.
    pub(super) async fn warn(
        &mut self,
        manager: &Manager,
        now: Instant,
        signal: Signal,
        message: &str,
    ) {
        let repeat =
            Duration::from_secs(manager.config().machine.overload.warning.repeat_minutes * 60);
        self.sent
            .retain(|_, sent| now.duration_since(*sent) < repeat);
        let delivery = signal.delivery();
        for workspace_id in manager.running_agent_workspaces().await {
            let key = (workspace_id, signal);
            if self.sent.contains_key(&key) {
                continue;
            }
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

/// Memory and CPU warnings report the machine's load without asking the
/// agent to change its work.
const HEADS_UP: &str = "This is information only; it needs no action from you.";

pub(super) fn memory(settings: &Overload) -> String {
    let level = if cfg!(target_os = "macos") {
        "Memory pressure is high: macOS reports warning pressure, and Shoal stops an agent at critical pressure.".to_owned()
    } else {
        format!(
            "Memory pressure is high: memory use is above {}%, and Shoal stops an agent at {}%.",
            settings.warning.memory_used_percent, settings.memory.used_percent
        )
    };
    format!("{level} {HEADS_UP}")
}

pub(super) fn cpu(settings: &Overload) -> String {
    format!(
        "CPU use has been above {}% for {} seconds; Shoal stops an agent after {} seconds above {}%. {HEADS_UP}",
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
