//! The leases Shoal takes back from asleep agents, which otherwise keep
//! them: one that keeps something running, a simulator or containers, once
//! its agent has slept past `[sleep] keep_leases_minutes` or the machine
//! needs its memory. The agent reads which it lost, why, and how to get it
//! back once it runs again.
use std::collections::HashMap;

use anyhow::Result;
use shoal_ids::space_ids::SpaceId;

use crate::acquired_leases::{Acquired, Held};
use crate::daemon::{Daemon, Sleeper};
use crate::{Leases, model::Workspace, sim::Device, time::UnixTime};

/// Why Shoal took a lease back from an asleep agent.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Taken {
    /// Its agent slept past its limit, in minutes.
    Overslept(u64),
    /// The machine ran short of memory.
    Memory,
}

impl std::fmt::Display for Taken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Overslept(minutes) => write!(f, "stopped after {minutes} minutes asleep"),
            Self::Memory => f.write_str("stopped to free memory"),
        }
    }
}

impl Leases {
    /// Stop the running leases of every agent asleep past how long it
    /// keeps them.
    pub async fn stop_leases_of_long_sleepers(&self, daemon: &dyn Daemon) -> Result<()> {
        let now = UnixTime::now();
        let overdue: HashMap<SpaceId, Sleeper> = (daemon.sleepers().await?.into_iter())
            .filter(|sleeper| sleeper.overdue(now))
            .map(|sleeper| (sleeper.space.clone(), sleeper))
            .collect();
        if overdue.is_empty() {
            return Ok(());
        }
        for lease in self.acquired_leases().await? {
            let sleeper = (lease.acquirer.agent_space()).and_then(|space| overdue.get(space));
            if let Some(sleeper) = sleeper.filter(|_| lease.held.running()) {
                let minutes = sleeper.keeps_running.as_secs() / 60;
                self.stop_running(daemon, &lease, Taken::Overslept(minutes))
                    .await;
            }
        }
        Ok(())
    }

    /// Stop one running lease of the lowest-ranked asleep agent that holds
    /// one, to free memory, returning whether one stopped.
    pub async fn stop_a_sleepers_running_lease(&self, daemon: &dyn Daemon) -> Result<bool> {
        let sleepers = daemon.sleepers().await?;
        let leases = self.acquired_leases().await?;
        for sleeper in &sleepers {
            let held = (leases.iter()).filter(|lease| {
                lease.held.running() && lease.acquirer.agent_space() == Some(&sleeper.space)
            });
            for lease in held {
                if self.stop_running(daemon, lease, Taken::Memory).await {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// Stop the running `lease` of an asleep agent for `why`, shutting a
    /// simulator down, and tell its agent; whether it stopped.
    async fn stop_running(&self, daemon: &dyn Daemon, lease: &Acquired, why: Taken) -> bool {
        let released = self.release_acquired_lease(daemon, lease, Device::ShutDown);
        let Some(workspace) = released.await else {
            return false;
        };
        tell_taken(daemon, lease, &workspace, why).await;
        true
    }
}

/// Tell the agent that acquired `lease` in `workspace` that Shoal took it
/// back for `why`, and how to get it back.
async fn tell_taken(
    daemon: &dyn Daemon,
    lease: &Acquired,
    workspace: &Workspace,
    why: impl std::fmt::Display,
) {
    let Some(space) = lease.acquirer.agent_space() else {
        return;
    };
    let message = taken_message(&lease.held, workspace, why);
    daemon.tell_agent(space, message).await;
}

/// What the agent reads of `held` in `workspace`, which Shoal took back
/// for `why` while it slept.
fn taken_message(held: &Held, workspace: &Workspace, why: impl std::fmt::Display) -> String {
    format!(
        "While you slept, Shoal took back your {held} in space {}: {why}. `{}` gets it back.",
        workspace.number,
        held.acquire_command(workspace.number)
    )
}

#[cfg(test)]
mod tests;
