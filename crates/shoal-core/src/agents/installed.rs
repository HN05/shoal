//! The agents a machine can launch: those Shoal knows or the settings name,
//! less those the settings turn off, and among them the ones whose every
//! program is installed.
use std::ffi::OsStr;

use super::{ADDED, Added, agent_off, programs};
use crate::{
    agent::Agent,
    config::{Effective, named_commands::Commands},
    tools::program_available,
};

/// The agents pickers and completion offer under `settings`, built-in
/// providers first, leaving out those `settings` turn off.
pub fn names(settings: &Effective) -> Vec<Agent> {
    let mut names: Vec<String> = crate::ai::PROVIDERS
        .iter()
        .map(|provider| provider.name.to_owned())
        .collect();
    for name in crate::ai::agents(&settings.tools) {
        if !names.iter().any(|known| known == name) {
            names.push(name.to_owned());
        }
    }
    for Added { name, .. } in ADDED {
        if !names.iter().any(|known| known == name) {
            names.push((*name).to_owned());
        }
    }
    let mut agents: Vec<Agent> = names.iter().filter_map(|name| name.parse().ok()).collect();
    agents.retain(|agent| agent_off(settings, agent).is_none());
    agents
}

/// Whether every executable the agent itself starts is available, so pickers
/// and completion offer only agents that can launch; a launch refuses a
/// missing session host itself.
pub fn installed(name: &Agent, commands: &Commands, search_path: &OsStr) -> bool {
    programs(name, commands, None).is_some_and(|programs| {
        programs
            .into_iter()
            .all(|program| program_available(program, search_path))
    })
}

/// The agents of [`names`] under `settings` that are [`installed`] on
/// `search_path`.
pub fn installed_agents(settings: &Effective, search_path: &OsStr) -> Vec<Agent> {
    let commands = &settings.launchers;
    (names(settings).into_iter())
        .filter(|name| installed(name, commands, search_path))
        .collect()
}

#[cfg(test)]
mod tests;
