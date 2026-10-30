//! `[agents.<name>]`: how each AI tool launches, merged field by field across
//! layers, and `[machine.agents.<name>]`: what it needs of this machine.
use std::{collections::BTreeMap, path::PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{agent::AgentMode, config::options::Options};

#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = AgentTool)]
pub struct AgentToolConfig {
    /// The launcher `shoal <name>` and tracked launches run; a tool with one
    /// runs as an agent.
    pub command: Option<Vec<String>>,
    /// Restores a session; it must not repeat the initial prompt.
    pub resume_command: Option<Vec<String>>,
    /// What `shoal <name>` does without `--cli` or `--app`.
    #[option(default = AgentMode::default())]
    pub default_mode: Option<AgentMode>,
    /// The AI tool this one is a launch profile of: it inherits that tool's
    /// integration, launcher and arguments, each replaced by its own.
    pub base: Option<String>,
    /// Arguments that give the agent instructions apart from its first
    /// message, with `{instructions}`, or `{instructions_toml}` for a TOML
    /// string. Without them, instructions join the prompt.
    pub instructions: Option<Vec<String>>,
    /// Arguments that resume the session with ID `{session}`.
    pub resume: Option<Vec<String>>,
    /// Arguments that resume the workspace's latest session.
    pub resume_latest: Option<Vec<String>>,
    /// Arguments that start a new session from a fork of the one with ID
    /// `{session}`, which stays unchanged, working in `{directory}`.
    pub fork: Option<Vec<String>>,
    /// Arguments that give the agent another working directory,
    /// `{directory}`; empty gives none.
    pub add_dir: Option<Vec<String>>,
    /// Variables set for the tool's tracked launches, after the workspace's
    /// `[env]`; a value starting with `~/` names a path under the home.
    #[option(named)]
    pub env: BTreeMap<String, String>,
    /// Variables removed from the tool's tracked launches, whether inherited
    /// or set by `[env]`.
    #[option(list)]
    pub env_remove: Vec<String>,
    /// Whether Shoal retries a turn an API failure ended, for an agent whose
    /// integration reports failed turns.
    pub retry: Option<bool>,
    /// How many minutes the tool's prompt cache lives after its last
    /// request, over what its own settings give.
    pub prompt_cache_minutes: Option<u64>,
}

/// What an AI tool needs of this machine.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MachineAgent {
    /// Parent directory in which Shoal installs `<skill>/SKILL.md`.
    pub skill_dir: Option<PathBuf>,
    /// The `[machine.proxies.<name>]` the tool's tracked agents reach their
    /// API through.
    pub proxy: Option<String>,
}

/// One layer's agents on their own: each entry's names, commands, argument
/// templates and variables. Bases and the rest resolve across layers, which
/// [`crate::ai::validate`] checks once they combine.
pub fn validate(agents: &BTreeMap<String, AgentToolConfig>) -> Result<()> {
    for (name, agent) in agents {
        if agent.command.is_some() || agent.base.is_some() {
            crate::ai::validate::reachable_name(name).with_context(|| format!("agents.{name}"))?;
        }
        for argv in [&agent.command, &agent.resume_command]
            .into_iter()
            .flatten()
        {
            let command = BTreeMap::from([(name.clone(), argv.clone())]);
            crate::config::named_commands::validate(&command)
                .with_context(|| format!("agents.{name}"))?;
        }
    }
    let resolved = crate::config::options::resolve_sections(agents.clone(), "agents")?;
    for (name, agent) in describe(&resolved, &BTreeMap::new()) {
        crate::ai::validate::entry(&name, &agent)?;
    }
    Ok(())
}

/// The built-in AI tools' launchers, the lowest layer of `[agents]`.
pub fn built_in() -> BTreeMap<String, AgentToolConfig> {
    crate::ai::default_commands()
        .into_iter()
        .map(|(name, command)| {
            let agent = AgentToolConfig {
                command: Some(command),
                ..Default::default()
            };
            (name, agent)
        })
        .collect()
}

/// Each AI tool described by its resolved settings and this machine's.
pub fn describe(
    agents: &BTreeMap<String, AgentTool>,
    machine: &BTreeMap<String, MachineAgent>,
) -> crate::ai::Agents {
    let names = agents.keys().chain(machine.keys());
    names
        .map(|name| {
            let settings = agents.get(name);
            let local = machine.get(name).cloned().unwrap_or_default();
            let described = crate::ai::Agent {
                skill_dir: local.skill_dir,
                proxy: local.proxy,
                command: settings.and_then(|agent| agent.command.clone()),
                instructions: settings.and_then(|agent| agent.instructions.clone()),
                resume: settings.and_then(|agent| agent.resume.clone()),
                resume_latest: settings.and_then(|agent| agent.resume_latest.clone()),
                fork: settings.and_then(|agent| agent.fork.clone()),
                add_dir: settings.and_then(|agent| agent.add_dir.clone()),
                base: settings.and_then(|agent| agent.base.clone()),
                env: settings.map(|agent| agent.env.clone()).unwrap_or_default(),
                env_remove: settings
                    .map(|agent| agent.env_remove.clone())
                    .unwrap_or_default(),
                retry: settings.and_then(|agent| agent.retry),
                prompt_cache_minutes: settings.and_then(|agent| agent.prompt_cache_minutes),
            };
            (name.clone(), described)
        })
        .collect()
}
