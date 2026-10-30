//! Settings that describe this machine: `[machine]` in the global config,
//! which no repository overrides.
use std::path::PathBuf;

use serde::Deserialize;

use super::{AgentLimit, forges, overload};

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Machine {
    /// Parent of every repository directory.
    pub root_dir: Option<PathBuf>,
    /// How hard Shoal may load this machine; sets `[machine.overload]` defaults.
    pub load: overload::Load,
    /// Hold off sleep, including from a closed laptop lid, while agents have work.
    pub keep_awake: bool,
    /// Agents running at once on this machine; defaults to measured load and
    /// `shoal add` queues past it. Use `none` for unlimited admission.
    pub max_agents: AgentLimit,
    pub overload: overload::Overload,
    /// What each AI tool needs of this machine.
    pub agents: std::collections::BTreeMap<String, super::agents::MachineAgent>,
    /// Retrying proxies an AI tool's agents reach their API through.
    pub proxies: crate::proxy::Proxies,
    /// Forge hosts whose type is set instead of asked.
    pub forges: forges::Forges,
    pub bitbucket: crate::forge::backends::bitbucket::Credentials,
    /// Named Git settings a repository or agent selects.
    pub git_profiles: crate::git_profile::Git,
    pub simulators: crate::sim::SimConfig,
}
