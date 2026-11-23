//! The leases Shoal takes back from asleep agents, which otherwise keep
//! them: one that keeps something running, a simulator or containers, once
//! its agent has slept past `[sleep] keep_leases_minutes` or the machine
//! needs its memory, and one other requests can find busy, a permit or a
//! simulator, for the request that needs it. The agent reads which it lost,
//! why, and how to get it back once it runs again.
use std::collections::HashMap;

use anyhow::Result;
use shoal_ids::space_ids::SpaceId;

use crate::acquired_leases::{Acquired, Held};
use crate::daemon::{Daemon, Sleeper};
use crate::{Leases, callers::Caller, model::Workspace, sim::Device, time::UnixTime};
use shoal_ids::space_ids::SpaceNumber;
use shoal_protocol::model::{KeptKind, KeptLease};

/// Why Shoal took a lease back from an asleep agent.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Taken {
    /// Its agent slept past its limit, in minutes.
    Overslept(u64),
    /// The machine ran short of memory.
    Memory,
    /// A request for the space numbered so needed it.
    Given(SpaceNumber),
}

impl std::fmt::Display for Taken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Overslept(minutes) => write!(f, "stopped after {minutes} minutes asleep"),
            Self::Memory => f.write_str("stopped to free memory"),
            Self::Given(number) => write!(f, "given to space {number}"),
        }
    }
}

impl Leases {
    /// Stop the running leases of every agent asleep past how long it
    /// keeps them.
    pub async fn stop_leases_of_long_sleepers(&self, daemon: &dyn Daemon) -> Result<()> {
        let now = UnixTime::now();
        let overdue: HashMap<SpaceId, Sleeper> = (daemon.sleepers(None).await?.into_iter())
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
        let sleepers = daemon.sleepers(None).await?;
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

    /// Release one of the sets of leases in `freeing`, doing what `device`
    /// says with a simulator's, each of which would grant a request for
    /// `workspace` from `caller` that capacity keeps busy, once every lease
    /// in it is an asleep agent's that may yield to
    /// the request: any for the user's, else one whose space's priority is
    /// no higher than the workspace's. The set whose highest-ranked holder
    /// ranks lowest goes, the smallest among equals. Whether it went.
    pub(crate) async fn give_to_request(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        caller: Option<&Caller>,
        freeing: Vec<Vec<Acquired>>,
        device: Device,
    ) -> bool {
        let requester = caller.map(|_| &workspace.id);
        let sleepers = match daemon.sleepers(requester).await {
            Ok(sleepers) => sleepers,
            Err(error) => {
                crate::log!("asleep agents unread for a busy request: {error:#}");
                return false;
            }
        };
        let Some(given) = lowest_ranked(&sleepers, freeing) else {
            return false;
        };
        for lease in &given {
            let released = self.release_acquired_lease(daemon, lease, device);
            let Some(holding) = released.await else {
                return false;
            };
            tell_taken(daemon, lease, &holding, Taken::Given(workspace.number)).await;
        }
        true
    }

    /// The leases the asleep agent of the space `id` keeps, each running
    /// one with when it stops; none while its agent is not asleep.
    pub async fn kept_by(&self, daemon: &dyn Daemon, id: &SpaceId) -> Result<Vec<KeptLease>> {
        let sleepers = daemon.sleepers(None).await?;
        let Some(sleeper) = sleepers.iter().find(|sleeper| sleeper.space == *id) else {
            return Ok(Vec::new());
        };
        let leases = self.acquired_leases().await?.into_iter();
        Ok(
            (leases.filter(|lease| lease.acquirer.agent_space() == Some(id)))
                .map(|lease| kept(lease.held, sleeper))
                .collect(),
        )
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

/// `held` as `sleeper` keeps it.
fn kept(held: Held, sleeper: &Sleeper) -> KeptLease {
    let until = (held.running()).then(|| sleeper.since + sleeper.keeps_running);
    let (kind, name) = match held {
        Held::Simulator { name } => (KeptKind::Simulator, Some(name)),
        Held::Containers => (KeptKind::Containers, None),
        Held::Permit { pool, .. } => (KeptKind::Resource, Some(pool)),
    };
    KeptLease { kind, name, until }
}

/// The set in `freeing` held wholly by `sleepers`, lowest-ranked first,
/// whose highest-ranked holder ranks lowest, the smallest among equals.
fn lowest_ranked(sleepers: &[Sleeper], freeing: Vec<Vec<Acquired>>) -> Option<Vec<Acquired>> {
    let rank = |lease: &Acquired| {
        let space = lease.acquirer.agent_space()?;
        sleepers.iter().position(|sleeper| sleeper.space == *space)
    };
    (freeing.into_iter())
        .filter_map(|leases| {
            let highest = leases.iter().map(rank).collect::<Option<Vec<_>>>()?;
            Some(((highest.into_iter().max()?, leases.len()), leases))
        })
        .min_by_key(|(order, _)| *order)
        .map(|(_, leases)| leases)
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
