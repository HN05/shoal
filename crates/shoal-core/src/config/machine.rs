//! Settings that describe this machine: `[machine]` in the global config,
//! which no repository overrides.
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{forges, overload, same_as};

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

shoal_states::states! {
    /// `keep_awake`: what this machine does once the user walks away from
    /// it, by closing its lid or leaving it idle.
    #[derive(Default)]
    KeepAwake: Variants {
        /// Sleep as usual.
        #[default]
        Off => "off",
        /// Admit nothing new and hand the spaces with something to do over
        /// to other machines, then sleep.
        Handover => "handover",
        /// Admit nothing new and let running turns finish, then sleep.
        Finish => "finish",
        /// Carry on as while the user is there.
        On => "on",
    }
}

impl KeepAwake {
    /// Whether this machine admits nothing new once the user walks away.
    pub fn stops_when_away(self) -> bool {
        matches!(self, Self::Handover | Self::Finish)
    }

    /// A level by its name, refusing any other value, such as the boolean
    /// an earlier Shoal took, with the names it takes.
    fn deserialize<'de, D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let name = String::deserialize(deserializer).ok();
        let level = Self::ALL
            .iter()
            .find(|level| Some(level.as_str()) == name.as_deref());
        level.copied().ok_or_else(|| {
            let names: Vec<String> = Self::ALL
                .iter()
                .map(|level| format!("\"{level}\""))
                .collect();
            serde::de::Error::custom(format!("keep_awake takes {}", names.join(", ")))
        })
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Machine {
    /// Parent of every repository directory.
    pub root_dir: Option<PathBuf>,
    /// How hard Shoal may load this machine; sets `[machine.overload]` defaults.
    pub load: overload::Load,
    /// What this machine does once the user walks away from it.
    #[serde(deserialize_with = "KeepAwake::deserialize")]
    pub keep_awake: KeepAwake,
    /// Whether this machine stays reachable for lasting work, which the hub
    /// guesses under `auto`.
    pub availability: shoal_protocol::hub::availability::AvailabilitySetting,
    /// Heavy slots: heavy modes' agents running at once on this machine;
    /// defaults to measured load and `shoal start` queues past it.
    pub max_heavy_agents: MachineLimit,
    /// CI jobs the runner runs at once on this machine; defaults to measured
    /// load, which agents and jobs share.
    pub max_jobs: MachineLimit,
    /// What the agent queue starts first after priority.
    pub schedule: Schedule,
    /// The most memory Shoal's work holds together; no limit without it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_limit: Option<super::memory_limit::MemoryLimit>,
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

/// How many heavy agents, or CI jobs, this machine runs at once: as
/// measured load allows, or a fixed number, where 0 admits none.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MachineLimit {
    #[default]
    Dynamic,
    Fixed(u32),
}

impl MachineLimit {
    /// The fixed limit, none under dynamic admission.
    pub fn fixed(self) -> Option<u32> {
        match self {
            Self::Dynamic => None,
            Self::Fixed(limit) => Some(limit),
        }
    }
}

impl Serialize for MachineLimit {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Dynamic => serializer.serialize_str("dynamic"),
            Self::Fixed(limit) => serializer.serialize_u32(*limit),
        }
    }
}

impl<'de> Deserialize<'de> for MachineLimit {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Count(u32),
            Name(String),
        }

        match Raw::deserialize(deserializer) {
            Ok(Raw::Count(limit)) => Ok(Self::Fixed(limit)),
            Ok(Raw::Name(name)) if name == "dynamic" => Ok(Self::Dynamic),
            _ => Err(serde::de::Error::custom(
                "the limit must be 'dynamic' or a number from 0 up",
            )),
        }
    }
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
    /// Install the hub's exact version once the hub refuses this machine's,
    /// where Shoal was installed from a release download.
    pub auto_update: bool,
    /// How long a thread or approval request waits for a coordinator
    /// whose machine is offline before it passes to the next space above
    /// that is online, or to the user.
    pub escalate_after_minutes: u64,
    /// The machine, by name or ID, a device's start goes to when it names
    /// none, on the hub; none sends it where the device sent it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_machine: Option<String>,
    /// The policy that places a start which names no machine, on the hub.
    pub placement: shoal_schedule::placement::Policy,
    /// The local address the hub forwards the requests of paired members
    /// under each path prefix to, such as an app's own server.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub forward: std::collections::BTreeMap<
        shoal_protocol::hub::app_servers::PathPrefix,
        std::net::SocketAddr,
    >,
}

impl Default for Hub {
    fn default() -> Self {
        Self {
            enabled: false,
            listen: std::net::SocketAddr::from(([127, 0, 0, 1], 7474)),
            url: None,
            connect: Vec::new(),
            auto_update: true,
            escalate_after_minutes: 30,
            default_machine: None,
            placement: Default::default(),
            forward: Default::default(),
        }
    }
}

impl Hub {
    /// The prefix and address of the longest prefix of `forward` that
    /// covers `path`.
    pub fn forwarded(
        &self,
        path: &str,
    ) -> Option<(
        &shoal_protocol::hub::app_servers::PathPrefix,
        std::net::SocketAddr,
    )> {
        (self.forward.iter())
            .filter(|(prefix, _)| prefix.covers(path))
            .max_by_key(|(prefix, _)| prefix.as_str().len())
            .map(|(prefix, address)| (prefix, *address))
    }

    /// The URL the hub advertises for a listener bound to `bound`.
    pub fn advertised(&self, bound: std::net::SocketAddr) -> shoal_protocol::hub::HubUrl {
        (self.url.clone()).unwrap_or_else(|| shoal_protocol::hub::HubUrl::of_listener(bound))
    }
}

/// `[machine]` as one file writes it: the keys it sets, with its `same_as`
/// references taken out.
#[derive(Debug, Default, Clone)]
pub(crate) struct Written {
    table: toml::Table,
    same_as: same_as::References,
}

impl Written {
    pub(crate) fn new(table: toml::Table, same_as: same_as::References) -> Self {
        Self { table, same_as }
    }

    /// What this table sets over the defaults, its references not followed.
    pub(crate) fn read(&self) -> Result<Machine> {
        let machine: Machine = self.table.clone().try_into()?;
        Ok(Machine {
            same_as: self.same_as.clone(),
            ..machine
        })
    }

    /// This table over `below`: each key it sets wins, a table merging key
    /// by key with the one below it, and each reference of `below` stays
    /// unless this table sets its key or names another value for it.
    fn over(self, below: Self) -> Result<Self> {
        let same_as = (self.same_as).over(&serde_json::to_value(&self.table)?, below.same_as);
        let mut table = below.table;
        merge_tables(&mut table, self.table);
        Ok(Self { table, same_as })
    }
}

/// Layer `top` over `base`: a table merges with the table below it, and any
/// other value replaces what is below.
fn merge_tables(base: &mut toml::Table, top: toml::Table) {
    for (key, value) in top {
        match (base.get_mut(&key), value) {
            (Some(toml::Value::Table(below)), toml::Value::Table(above)) => {
                merge_tables(below, above)
            }
            (_, value) => {
                base.insert(key, value);
            }
        }
    }
}

impl Machine {
    /// `[machine]` of this machine's own global file over the shared
    /// file's, with each `same_as` reference followed and the overload
    /// thresholds the files leave out following `load`.
    pub(crate) fn layered(shared: Written, own: Written) -> Result<Self> {
        let written = own.over(shared)?;
        let mut machine = written.read()?;
        let overload = written
            .table
            .get("overload")
            .and_then(toml::Value::as_table);
        (machine.overload).follow_load(machine.load, overload.unwrap_or(&toml::Table::new()));
        machine.follow_references()?;
        Ok(machine)
    }

    /// Give each key a `same_as` reference names its target's value, over
    /// the defaults of what the file leaves out.
    fn follow_references(&mut self) -> Result<()> {
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
