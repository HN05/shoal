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
mod tests {
    use super::*;
    use crate::{
        daemon::workspace::ExecutionKind,
        test_support::{manager, repository},
    };

    #[tokio::test]
    async fn running_agents_are_warned_once_per_signal_until_the_repeat_interval() {
        let (root, manager) = manager().await;
        let repo = repository(root.path(), "repo");
        let repo = manager
            .register_repository(repo.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let mut agents = Vec::new();
        for name in ["first", "second", "idle"] {
            let workspace = manager
                .create_workspace(&repo.id, name.into(), None, None, None)
                .await
                .unwrap();
            if name != "idle" {
                let execution = manager
                    .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
                    .await
                    .unwrap();
                // The execution's stop receiver keeps the agent connected.
                agents.push((workspace, execution));
            }
        }
        let track = |index: usize| {
            let (workspace, execution) = &agents[index];
            manager.track_agent(
                &execution.plan.id,
                "claude",
                &workspace.id,
                &workspace.name,
                false,
                Duration::ZERO,
            )
        };
        let settings = Overload::default();
        let count = |name: &'static str| {
            let manager = manager.clone();
            async move { manager.agent_messages(name).await.unwrap().len() }
        };
        let mut warnings = Warnings::default();
        let start = Instant::now();
        let later = start + Duration::from_secs(60);
        track(0).await;
        warnings
            .warn(&manager, start, Signal::Memory, &memory(&settings))
            .await;
        let delivered = manager.agent_messages("first").await.unwrap();
        manager
            .mark_agent_messages_delivered("first", vec![delivered[0].id])
            .await
            .unwrap();
        // Within the interval, only an agent started since is warned again,
        // and each signal has its own interval.
        track(1).await;
        warnings
            .warn(&manager, later, Signal::Memory, &memory(&settings))
            .await;
        warnings
            .warn(
                &manager,
                later,
                Signal::Disk,
                &disk(&settings, "1.0 GiB available at /"),
            )
            .await;
        assert_eq!(count("first").await, 1);
        assert_eq!(count("second").await, 2);
        assert_eq!(count("idle").await, 0);
        // After the interval, a persisting condition warns again.
        let repeat = Duration::from_secs(settings.warning.repeat_minutes * 60);
        warnings
            .warn(&manager, start + repeat, Signal::Memory, &memory(&settings))
            .await;
        assert_eq!(count("first").await, 2);
    }

    #[test]
    fn warnings_are_single_lines_within_the_message_limit() {
        let settings = Overload::default();
        let path = format!("/{}", "x".repeat(200));
        for message in [
            memory(&settings),
            cpu(&settings),
            disk(&settings, &format!("3.2 GiB available at {path}")),
        ] {
            crate::validate::message(&message).unwrap();
        }
    }
}
