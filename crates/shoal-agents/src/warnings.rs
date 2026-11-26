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

/// A warning's text: the load it reports and, where the agent of a
/// workspace can relieve it in its own worktree, how.
pub struct Warning {
    load: String,
    remedy: Option<String>,
}

impl Warning {
    /// Its text for the agent of a space of `kind`, and how it reaches the
    /// agent while idle. A remedy wakes a workspace's agent to act on it;
    /// otherwise, and always for a chat, which has no worktree, it is a
    /// heads-up that asks for nothing and waits for the agent's next turn.
    fn for_space(&self, kind: SpaceKind) -> (String, Delivery) {
        match (&self.remedy, kind) {
            (Some(remedy), SpaceKind::Workspace) => {
                (format!("{} {remedy}", self.load), Delivery::Wake)
            }
            _ => (format!("{} {HEADS_UP}", self.load), Delivery::NextTurn),
        }
    }
}

/// When each space was last warned about each signal.
#[derive(Default)]
pub struct Warnings {
    sent: HashMap<(SpaceId, Signal), Instant>,
}

impl Warnings {
    /// Queue `warning` for each space with a running agent that has not
    /// been warned about `signal` within the repeat interval, including agents
    /// started since the last warning, as [`Warning::for_space`] gives it for
    /// the space's kind.
    pub async fn warn(&mut self, agents: &Agents, now: Instant, signal: Signal, warning: &Warning) {
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
            let (message, delivery) = warning.for_space(kind);
            if let Err(error) = agents
                .queue_agent_message_delivered(
                    &key.0,
                    shoal_daemon_base::alerts::AlertKind::LoadWarning,
                    message,
                    delivery,
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

/// What a warning without a remedy tells the agent: that it reports the
/// machine's load without asking the agent to change its work.
const HEADS_UP: &str = "This is information only; it needs no action from you.";

/// The warning of memory pressure, the machine's or, where `limited`,
/// that of Shoal's work against its memory limit.
pub fn memory(settings: &Overload, limited: bool) -> Warning {
    let used = match limited {
        true => "RAM and disk swap, or Shoal's work against its memory limit,",
        false => "RAM and disk swap",
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
    Warning {
        load: level,
        remedy: None,
    }
}

pub fn cpu(settings: &Overload) -> Warning {
    Warning {
        load: format!(
            "CPU use has been above {}% for {} seconds; Shoal stops an agent after {} seconds above {}%.",
            settings.warning.cpu_used_percent,
            settings.warning.cpu_sustained_seconds,
            settings.cpu.sustained_seconds,
            settings.cpu.used_percent,
        ),
        remedy: None,
    }
}

/// `available` describes the filesystem with the least free space.
pub fn disk(settings: &Overload, available: &str) -> Warning {
    Warning {
        load: format!(
            "Free disk space is low: {available}; Shoal stops running agents and tracked commands below {} GiB.",
            settings.disk.stop_free_gib,
        ),
        remedy: Some(format!(
            "Delete build output you no longer need in your own worktree, a whole directory or with its clean command, and avoid large downloads; leave other worktrees and Shoal's {} and {} alone.",
            Contents::Caches.directory(),
            Contents::Seed.directory(),
        )),
    }
}

#[cfg(test)]
mod tests;
