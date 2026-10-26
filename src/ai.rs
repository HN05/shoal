//! AI tools: the providers Shoal ships and the user's `[ai.<name>]` additions.
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::Deserialize;

mod builtin;
mod launch;
mod validate;

pub use builtin::{PROVIDERS, default_commands};
use builtin::{Provider, SkillDir};
pub use launch::{Arguments, Environment};
use validate::skill_dir;
pub use validate::validate;

pub type Agents = BTreeMap<String, Agent>;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Agent {
    /// Parent directory in which Shoal installs `<skill>/SKILL.md`.
    pub skill_dir: Option<PathBuf>,
    /// Launcher at the global layer; a provider with one runs as `shoal <name>`.
    pub command: Option<Vec<String>>,
    /// Arguments that give the agent instructions apart from its first
    /// message, with `{instructions}`, or `{instructions_toml}` for a TOML
    /// string. Without them, instructions join the prompt.
    pub instructions: Option<Vec<String>>,
    /// Arguments that resume the session with ID `{session}`.
    pub resume: Option<Vec<String>>,
    /// Arguments that resume the workspace's latest session.
    pub resume_latest: Option<Vec<String>>,
    /// Arguments that give the agent another working directory,
    /// `{directory}`; empty gives none.
    pub add_dir: Option<Vec<String>>,
    /// The AI tool this one is a launch profile of: it inherits that tool's
    /// integration, launcher and arguments, each replaced by its own.
    pub base: Option<String>,
    /// Variables set for the tool's tracked launches, after the workspace's
    /// `[env]`; a value starting with `~/` names a path under the home.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Variables removed from the tool's tracked launches, whether inherited
    /// or set by `[env]`.
    #[serde(default)]
    pub env_remove: Vec<String>,
    /// Whether Shoal retries a turn an API failure ended, for an agent whose
    /// integration reports failed turns.
    pub retry: Option<bool>,
}

/// Launchers configured under `[ai]`, which join the global `[commands]`.
pub fn commands(agents: &Agents) -> impl Iterator<Item = (String, Vec<String>)> + '_ {
    agents
        .iter()
        .filter_map(|(name, agent)| Some((name.clone(), agent.command.clone()?)))
}

/// Every AI tool that runs as an agent: built-in, or configured with a
/// launcher or a base. A tool with only a skill directory is not one.
pub fn providers(agents: &Agents) -> BTreeSet<String> {
    PROVIDERS
        .iter()
        .map(|provider| provider.name.to_owned())
        .chain(
            agents
                .iter()
                .filter(|(_, agent)| agent.command.is_some() || agent.base.is_some())
                .map(|(name, _)| name.clone()),
        )
        .collect()
}

/// `name` and the tools it is a launch profile of, nearest first.
/// Validation rejects cycles; the bound keeps an unvalidated one finite.
fn lineage<'a>(agents: &'a Agents, name: &'a str) -> impl Iterator<Item = &'a str> {
    std::iter::successors(Some(name), |name| agents.get(*name)?.base.as_deref())
        .take(agents.len() + 1)
}

/// The tool whose integration `name` runs with: the end of its lineage.
pub fn integration<'a>(agents: &'a Agents, name: &'a str) -> &'a str {
    lineage(agents, name).last().unwrap_or(name)
}

/// Whether Shoal retries `name`'s failed turns: the `retry` set nearest in
/// its lineage, defaulting to on for the Claude integration.
pub fn retries(agents: &Agents, name: &str) -> bool {
    lineage(agents, name)
        .find_map(|name| agents.get(name)?.retry)
        .unwrap_or_else(|| integration(agents, name) == "claude")
}

/// The tool each launch profile runs with the integration of.
pub fn integrations(agents: &Agents) -> BTreeMap<String, String> {
    agents
        .iter()
        .filter(|(_, agent)| agent.base.is_some())
        .map(|(name, _)| (name.clone(), integration(agents, name).to_owned()))
        .collect()
}

/// Give each launch profile without a launcher of its own in `commands` the
/// nearest one of its lineage there, so it follows whatever layer set it.
/// `commands` may carry each launcher's layer with it.
pub fn inherit_launchers<T: Clone>(agents: &Agents, commands: &mut BTreeMap<String, T>) {
    for name in agents.keys() {
        if commands.contains_key(name) {
            continue;
        }
        let inherited = lineage(agents, name)
            .skip(1)
            .find_map(|base| commands.get(base))
            .cloned();
        if let Some(argv) = inherited {
            commands.insert(name.clone(), argv);
        }
    }
}

/// The environment changes of every tool configured with any. A profile's
/// own entries win over its base's.
pub fn environments(agents: &Agents) -> BTreeMap<String, Environment> {
    agents
        .keys()
        .map(|name| (name.clone(), environment(agents, name)))
        .filter(|(_, environment)| *environment != Environment::default())
        .collect()
}

fn environment(agents: &Agents, name: &str) -> Environment {
    let lineage: Vec<_> = lineage(agents, name)
        .filter_map(|name| agents.get(name))
        .collect();
    let mut environment = Environment::default();
    for agent in lineage.into_iter().rev() {
        for variable in &agent.env_remove {
            environment.set.remove(variable);
            environment.remove.insert(variable.clone());
        }
        for (variable, value) in &agent.env {
            environment.remove.remove(variable);
            environment.set.insert(variable.clone(), value.clone());
        }
    }
    environment
}

/// The launcher arguments of every built-in or configured AI tool.
pub fn all_arguments(agents: &Agents) -> BTreeMap<String, Arguments> {
    PROVIDERS
        .iter()
        .map(|provider| provider.name)
        .chain(agents.keys().map(String::as_str))
        .map(|name| (name.to_owned(), arguments(agents, name)))
        .collect()
}

/// The arguments `name`'s launcher takes: each entry configured nearest in
/// its lineage, else the built-in provider's at its end.
pub fn arguments(agents: &Agents, name: &str) -> Arguments {
    let root = integration(agents, name);
    let builtin = PROVIDERS.iter().find(|provider| provider.name == root);
    let owned = |argv: &[&str]| argv.iter().map(|arg| arg.to_string()).collect();
    let pick = |configured: fn(&Agent) -> Option<&Vec<String>>, builtin: Option<&[&str]>| {
        lineage(agents, name)
            .find_map(|name| agents.get(name).and_then(configured))
            .cloned()
            .or_else(|| builtin.map(owned))
    };
    Arguments {
        instructions: pick(
            |agent| agent.instructions.as_ref(),
            builtin.and_then(|provider| provider.instructions),
        ),
        resume: pick(
            |agent| agent.resume.as_ref(),
            builtin.and_then(|provider| provider.resume),
        ),
        resume_latest: pick(
            |agent| agent.resume_latest.as_ref(),
            builtin.and_then(|provider| provider.resume_latest),
        ),
        add_dir: pick(
            |agent| agent.add_dir.as_ref(),
            builtin.and_then(|provider| provider.add_dir),
        ),
    }
}

/// Each provider's user-level skill directory; `None` when it has none.
/// A tool's own or nearest inherited `skill_dir`, else the directory of the
/// built-in provider its lineage ends at, in the tool's environment.
pub fn skill_dirs(agents: &Agents, home: &Path) -> Result<BTreeMap<String, Option<PathBuf>>> {
    let names = PROVIDERS
        .iter()
        .map(|provider| provider.name)
        .chain(agents.keys().map(String::as_str));
    let mut directories = BTreeMap::new();
    for name in names {
        let configured = lineage(agents, name)
            .find_map(|name| agents.get(name)?.skill_dir.as_ref())
            .map(|directory| skill_dir(directory, home))
            .transpose()?;
        let directory = match configured {
            Some(directory) => Some(directory),
            None => PROVIDERS
                .iter()
                .find(|provider| provider.name == integration(agents, name))
                .map(|provider| {
                    provider_skill_dir(provider, &environment(agents, name).values(home), home)
                })
                .transpose()?,
        };
        directories.insert(name.to_owned(), directory);
    }
    Ok(directories)
}

/// `provider`'s skill directory for a launch that adds `configured` to this
/// process's environment.
fn provider_skill_dir(
    provider: &Provider,
    configured: &BTreeMap<String, OsString>,
    home: &Path,
) -> Result<PathBuf> {
    Ok(match provider.skill_dir {
        SkillDir::Claude => crate::env::claude_config_dir_with(configured)?
            .unwrap_or_else(|| home.join(".claude"))
            .join("skills"),
        SkillDir::Home(relative) => home.join(relative),
    })
}

/// Skill delivery reads machine config without constructing daemon paths.
pub fn load(home: &Path) -> Result<Agents> {
    let path = crate::config::Config::path_for_home(home);
    let Some(text) =
        crate::fsutil::read_optional(&path).with_context(|| format!("read {}", path.display()))?
    else {
        return Ok(Agents::new());
    };
    let config: crate::config::Config =
        toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    validate(&config.ai, &config.commands, home)?;
    Ok(config.ai)
}

#[cfg(test)]
mod tests;
