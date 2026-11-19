//! Settings that describe this machine: `[machine]` in the global config,
//! which no repository overrides.
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{RunLimit, forges, overload, same_as};

shoal_states::states! {
    /// What the agent queue starts first within a priority.
    #[derive(Default)]
    Schedule {
        /// Work closest to finished, then the tie-breaks.
        #[default]
        Balanced => "balanced",
        /// The work with the longest chain of work waiting on it, then as
        /// `balanced`.
        Speed => "speed",
        /// Restores of agents whose prompt cache has not expired, the latest
        /// to expire first, then as `balanced`.
        Cache => "cache",
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Machine {
    /// Parent of every repository directory.
    pub root_dir: Option<PathBuf>,
    /// How hard Shoal may load this machine; sets `[machine.overload]` defaults.
    pub load: overload::Load,
    /// Hold off sleep, including from a closed laptop lid, while agents have work.
    pub keep_awake: bool,
    /// Heavy slots: heavy modes' agents running at once on this machine;
    /// defaults to measured load and `shoal start` queues past it. Use `none`
    /// for unlimited admission.
    pub max_heavy_agents: RunLimit,
    /// CI jobs the runner runs at once on this machine; defaults to measured
    /// load, which agents and jobs share. Use `none` for unlimited admission.
    pub max_jobs: RunLimit,
    /// What the agent queue starts first after priority.
    pub schedule: Schedule,
    pub overload: overload::Overload,
    /// What each AI tool needs of this machine.
    pub agents: std::collections::BTreeMap<String, super::agents::MachineAgent>,
    /// Retrying proxies an AI tool's agents reach their API through.
    pub proxies: crate::config::proxies::Proxies,
    /// Forge hosts: the type each serves, and the tokens that sign in to it.
    pub forges: forges::Forges,
    /// Named Git settings a repository or agent selects.
    pub git_profiles: crate::config::git_profiles::Git,
    pub simulators: crate::sim::SimConfig,
    pub webhooks: Webhooks,
    pub hub: Hub,
    pub app_store: AppStore,
    pub containers: Containers,
    /// The options of `[machine]` that take another's value.
    #[serde(skip)]
    pub same_as: same_as::References,
}

/// The daemon's HTTP listener for webhook triggers.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Webhooks {
    /// The address it listens on; none without it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub listen: Option<std::net::SocketAddr>,
}

/// `[machine.hub]`: the hub role this daemon takes, or the command it
/// reaches the hub it joins through.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Hub {
    /// Take the hub role, which the user's other machines and devices join.
    pub enabled: bool,
    /// The address the hub's listener binds, which Shoal never encrypts.
    pub listen: std::net::SocketAddr,
    /// The URL members reach the listener at, through the proxy or tunnel in
    /// front of it; defaults to `http://` and the listener's address.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<shoal_protocol::hub::HubUrl>,
    /// A command whose standard input and output reach the hub's daemon,
    /// which this machine joins and reaches it through instead of a URL.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub connect: Vec<String>,
    /// How long a question or approval request waits for a coordinator
    /// whose machine is offline before it passes to the next space above
    /// that is online, or to the user.
    pub escalate_after_minutes: u64,
}

impl Default for Hub {
    fn default() -> Self {
        Self {
            enabled: false,
            listen: std::net::SocketAddr::from(([127, 0, 0, 1], 7474)),
            url: None,
            connect: Vec::new(),
            escalate_after_minutes: 30,
        }
    }
}

impl Hub {
    /// The URL the hub advertises for a listener bound to `bound`.
    pub fn advertised(&self, bound: std::net::SocketAddr) -> shoal_protocol::hub::HubUrl {
        (self.url.clone()).unwrap_or_else(|| shoal_protocol::hub::HubUrl::of_listener(bound))
    }
}

impl Machine {
    /// Give each key a `same_as` reference names its target's value, over
    /// the defaults of what the file leaves out.
    pub(crate) fn follow_references(&mut self) -> Result<()> {
        let mut layers = [std::mem::take(self)];
        same_as::follow(
            &mut layers,
            |machine| &machine.same_as,
            |layers| Ok(serde_json::to_value(&layers[0])?),
            "machine",
        )?;
        let [machine] = layers;
        *self = machine;
        Ok(())
    }
}

/// `[machine.app_store]`: how much the entries of apps this daemon keeps
/// may hold, each and in all for one app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppStore {
    /// The most bytes one entry holds, in MiB.
    pub max_entry_mib: u64,
    /// The most bytes all of one app's entries hold, in MiB.
    pub max_app_mib: u64,
}

impl AppStore {
    /// The largest entry the hub's link carries whole.
    const MOST_ENTRY_MIB: u64 = 128;

    pub fn max_entry_bytes(&self) -> u64 {
        self.max_entry_mib << 20
    }

    pub fn max_app_bytes(&self) -> u64 {
        self.max_app_mib << 20
    }

    pub(crate) fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            (1..=Self::MOST_ENTRY_MIB).contains(&self.max_entry_mib),
            "machine.app_store.max_entry_mib must be between 1 and {}",
            Self::MOST_ENTRY_MIB
        );
        anyhow::ensure!(
            self.max_app_mib >= self.max_entry_mib,
            "machine.app_store.max_app_mib must be at least max_entry_mib"
        );
        Ok(())
    }
}

impl Default for AppStore {
    fn default() -> Self {
        Self {
            max_entry_mib: 16,
            max_app_mib: 256,
        }
    }
}

/// `[machine.containers]`: the engine this machine runs workspaces'
/// projects with.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Containers {
    /// Unset takes the first of Docker and Podman on `PATH`.
    pub runtime: Option<shoal_protocol::containers::ContainerRuntime>,
}
