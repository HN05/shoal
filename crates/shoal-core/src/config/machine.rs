//! `[machine]` in the global config: settings that describe this machine,
//! which no repository overrides. The configuration reference describes each.
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{
    forges,
    memory_limit::{self, MemoryLimitSetting},
    overload, same_as,
};

shoal_states::states! {
    #[derive(Default)]
    Schedule {
        #[default]
        Balanced => "balanced",
        Speed => "speed",
        Cache => "cache",
    }
}

shoal_states::states! {
    #[derive(Default)]
    KeepAwake: Variants {
        #[default]
        Off => "off",
        Handover => "handover",
        Finish => "finish",
        On => "on",
    }
}

impl KeepAwake {
    pub fn stops_when_away(self) -> bool {
        matches!(self, Self::Handover | Self::Finish)
    }

    /// A level by its name, refusing any other value with the names it takes.
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
    pub root_dir: Option<PathBuf>,
    pub load: overload::Load,
    #[serde(deserialize_with = "KeepAwake::deserialize")]
    pub keep_awake: KeepAwake,
    pub availability: shoal_protocol::hub::availability::AvailabilitySetting,
    pub max_heavy_agents: MachineLimit,
    pub max_jobs: MachineLimit,
    pub schedule: Schedule,
    #[serde(skip_serializing_if = "MemoryLimitSetting::follows_load")]
    pub memory_limit: MemoryLimitSetting,
    pub overload: overload::Overload,
    pub agents: std::collections::BTreeMap<String, super::agents::MachineAgent>,
    pub proxies: crate::config::proxies::Proxies,
    pub forges: forges::Forges,
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

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Webhooks {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub listen: Option<std::net::SocketAddr>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Hub {
    pub enabled: bool,
    pub listen: std::net::SocketAddr,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<shoal_protocol::hub::HubUrl>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub connect: Vec<String>,
    pub auto_update: bool,
    pub escalate_after_minutes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_machine: Option<String>,
    pub placement: shoal_schedule::placement::Policy,
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
    /// The limit on Shoal's work in effect on this machine.
    pub fn work_memory_limit(&self) -> memory_limit::InEffect {
        self.memory_limit.in_effect(self.load)
    }

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppStore {
    pub max_entry_mib: u64,
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

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Containers {
    pub runtime: Option<shoal_protocol::containers::ContainerRuntime>,
}
