//! `[agents.<name>]`: how each AI tool launches, merged field by field across
//! layers, and `[machine.agents.<name>]`: what it needs of this machine. The
//! configuration reference describes each option.
use std::{collections::BTreeMap, path::PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::options::Options;

#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = AgentTool)]
pub struct AgentToolConfig {
    pub command: Option<Vec<String>>,
    pub resume_command: Option<Vec<String>>,
    pub enabled: Option<bool>,
    pub base: Option<String>,
    pub instructions: Option<Vec<String>>,
    pub resume: Option<Vec<String>>,
    pub resume_latest: Option<Vec<String>>,
    pub fork: Option<Vec<String>>,
    pub add_dir: Option<Vec<String>>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub model_args: Option<Vec<String>>,
    pub effort_args: Option<Vec<String>>,
    pub review: Option<Vec<String>>,
    pub review_resume: Option<Vec<String>>,
    pub gate: Option<Vec<String>>,
    pub gate_resume: Option<Vec<String>>,
    #[option(named)]
    pub env: BTreeMap<String, String>,
    pub env_remove: Option<Vec<String>>,
    pub retry: Option<bool>,
    pub prompt_cache_minutes: Option<u64>,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct MachineAgent {
    pub skill_dir: Option<PathBuf>,
    pub proxy: Option<String>,
}

/// One layer's agents on their own: each entry's names, commands, argument
/// templates and variables. Bases and the rest resolve across layers, which
/// [`crate::ai::validate`] checks once they combine.
pub fn validate(agents: &BTreeMap<String, AgentToolConfig>) -> Result<()> {
    for (name, agent) in agents {
        if agent.command.is_some() || agent.base.is_some() {
            crate::ai::validate::agent_name(name).with_context(|| format!("agents.{name}"))?;
        }
        if let Some(base) = &agent.base {
            crate::ai::validate::agent_name(base).with_context(|| format!("agents.{name}.base"))?;
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
    for name in agents.keys() {
        ensure_no_cycle(agents, name)?;
    }
    let resolved = crate::config::options::resolve_sections(agents.clone(), "agents")?;
    for (name, agent) in describe(&resolved, &BTreeMap::new()) {
        crate::ai::validate::entry(&name, &agent)?;
    }
    Ok(())
}

/// Following bases within this layer from `name` never comes back to it; a
/// base in another layer ends the walk, and the combined layers check it.
fn ensure_no_cycle(agents: &BTreeMap<String, AgentToolConfig>, name: &str) -> Result<()> {
    let mut seen = std::collections::BTreeSet::from([name]);
    let mut next = agents.get(name).and_then(|agent| agent.base.as_deref());
    while let Some(base) = next {
        anyhow::ensure!(
            seen.insert(base),
            "agents.{name}.base: bases of {name:?} form a cycle through {base:?}"
        );
        next = agents.get(base).and_then(|agent| agent.base.as_deref());
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
                model: settings.and_then(|agent| agent.model.clone()),
                effort: settings.and_then(|agent| agent.effort.clone()),
                model_args: settings.and_then(|agent| agent.model_args.clone()),
                effort_args: settings.and_then(|agent| agent.effort_args.clone()),
                review: settings.and_then(|agent| agent.review.clone()),
                review_resume: settings.and_then(|agent| agent.review_resume.clone()),
                gate: settings.and_then(|agent| agent.gate.clone()),
                gate_resume: settings.and_then(|agent| agent.gate_resume.clone()),
                base: settings.and_then(|agent| agent.base.clone()),
                env: settings.map(|agent| agent.env.clone()).unwrap_or_default(),
                env_remove: settings
                    .and_then(|agent| agent.env_remove.clone())
                    .unwrap_or_default(),
                retry: settings.and_then(|agent| agent.retry),
                prompt_cache_minutes: settings.and_then(|agent| agent.prompt_cache_minutes),
            };
            (name.clone(), described)
        })
        .collect()
}
