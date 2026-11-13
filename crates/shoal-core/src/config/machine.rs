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
    /// Agents running at once on this machine; defaults to measured load and
    /// `shoal start` queues past it. Use `none` for unlimited admission.
    pub max_agents: RunLimit,
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
    /// The options of `[machine]` that take another's value.
    #[serde(skip)]
    pub same_as: same_as::References,
}

impl Machine {
    /// Give each key a `same_as` reference names its target's value, over
    /// the defaults of what the file leaves out.
    pub fn follow_references(&mut self) -> Result<()> {
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
