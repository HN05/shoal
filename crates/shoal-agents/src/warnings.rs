//! Message running agents when machine load nears a protection's stop
//! threshold, so they know of it before Shoal stops or cleans anything.
use shoal_ids::space_ids::SpaceId;
use std::{collections::HashMap, time::Duration};
use tokio::time::Instant;

use crate::{
    Agents, config::overload::Overload, log, messages::Delivery, model::SpaceKind,
    shoal_dirs::Contents,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Signal {
    Memory,
    Cpu,
    Disk,
}

impl Signal {
    /// How its warning reaches an idle agent in a space of `kind`: a disk
    /// warning wakes a workspace's agent to delete files in its worktree,
    /// while every other waits for the agent's next turn, a memory or CPU
    /// warning being a heads-up that asks for nothing and a chat having no
    /// worktree to delete files in.
    fn delivery(self, kind: SpaceKind) -> Delivery {
        match (self, kind) {
            (Self::Disk, SpaceKind::Workspace) => Delivery::Wake,
            _ => Delivery::NextTurn,
        }
    }
}

/// When each space was last warned about each signal.
#[derive(Default)]
pub struct Warnings {
    sent: HashMap<(SpaceId, Signal), Instant>,
}

impl Warnings {
    /// Queue `message` for each space with a running agent that has not
    /// been warned about `signal` within the repeat interval, including agents
    /// started since the last warning, delivered as [`Signal::delivery`] says
    /// for its space's kind.
    pub async fn warn(&mut self, agents: &Agents, now: Instant, signal: Signal, message: &str) {
        let repeat = Duration::from_secs(
            agents
                .config
                .current()
                .machine
                .overload
                .warning
                .repeat_minutes
                * 60,
        );
        self.sent
            .retain(|_, sent| now.duration_since(*sent) < repeat);
        for (space_id, kind) in running_agent_spaces(agents).await {
            let key = (space_id, signal);
            if self.sent.contains_key(&key) {
                continue;
            }
            if let Err(error) = agents
                .queue_agent_message_delivered(
                    &key.0,
                    shoal_daemon_base::alerts::AlertKind::LoadWarning,
                    message.into(),
                    signal.delivery(kind),
                )
                .await
            {
                log!("pressure warning not queued: {error:#}");
            }
            self.sent.insert(key, now);
        }
    }
}

/// Each space with a running agent, with its kind, while it is recorded;
/// none when the spaces cannot be read, so the next check warns them.
async fn running_agent_spaces(agents: &Agents) -> Vec<(SpaceId, SpaceKind)> {
    let running = agents.running_agent_workspaces().await;
    let records = agents.records.clone();
    let kinds = agents
        .store
        .read(move |db| {
            let kinds: HashMap<_, _> = (records.spaces(db)?.into_iter())
                .map(|space| (space.id, space.kind))
                .collect();
            Ok(kinds)
        })
        .await;
    match kinds {
        Ok(kinds) => (running.into_iter())
            .filter_map(|id| Some((id.clone(), *kinds.get(&id)?)))
            .collect(),
        Err(error) => {
            log!("pressure warning not queued: {error:#}");
            Vec::new()
        }
    }
}

/// Memory and CPU warnings report the machine's load without asking the
/// agent to change its work.
const HEADS_UP: &str = "This is information only; it needs no action from you.";

/// The warning of memory pressure, the machine's or, where `limited`,
/// that of Shoal's work against its memory limit.
pub fn memory(settings: &Overload, limited: bool) -> String {
    let used = match limited {
        true => "RAM and swap, or Shoal's work against its memory limit,",
        false => "RAM and swap",
    };
    let level = if cfg!(target_os = "macos") && !limited {
        "Memory pressure is high: macOS reports warning pressure, and Shoal stops an agent at critical pressure.".to_owned()
    } else if cfg!(target_os = "macos") {
        format!(
            "Memory pressure is high: macOS reports warning pressure or Shoal's work passed {}% of its memory limit, and Shoal stops an agent at critical pressure or {}% of the limit.",
            settings.warning.memory_used_percent, settings.memory.used_percent,
        )
    } else {
        format!(
            "Memory pressure is high: {used} in use passed {}% or memory stall {}%, and Shoal stops an agent at {}% in use or at {}% stall of every task.",
            settings.warning.memory_used_percent,
            settings.warning.memory_stall_percent,
            settings.memory.used_percent,
            settings.memory.stall_percent,
        )
    };
    format!("{level} {HEADS_UP}")
}

pub fn cpu(settings: &Overload) -> String {
    format!(
        "CPU use has been above {}% for {} seconds; Shoal stops an agent after {} seconds above {}%. {HEADS_UP}",
        settings.warning.cpu_used_percent,
        settings.warning.cpu_sustained_seconds,
        settings.cpu.sustained_seconds,
        settings.cpu.used_percent,
    )
}

/// `available` describes the filesystem with the least free space.
pub fn disk(settings: &Overload, available: &str) -> String {
    format!(
        "Free disk space is low: {available}; Shoal stops running agents and tracked commands below {} GiB. Delete build output you no longer need in your own worktree, a whole directory or with its clean command, and avoid large downloads; leave other worktrees and Shoal's {} and {} alone.",
        settings.disk.stop_free_gib,
        Contents::Caches.directory(),
        Contents::Seed.directory(),
    )
}

#[cfg(test)]
mod tests;
