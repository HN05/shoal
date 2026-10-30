//! AI tools: the providers Shoal ships and the user's `[machine.ai.<name>]` additions,
//! each resolved once into a [`Tool`].
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::Deserialize;

mod builtin;
mod launch;
mod resolve;
mod validate;

pub use builtin::{PROVIDERS, default_commands};
pub use launch::{Arguments, Environment};
pub use resolve::{DEFAULT_PROMPT_CACHE, IntegrationKind, LaunchProfile, Tools, inherit_launchers};
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
    /// Arguments that start a new session from a fork of the one with ID
    /// `{session}`, which stays unchanged, working in `{directory}`.
    pub fork: Option<Vec<String>>,
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
    /// How many minutes the tool's prompt cache lives after its last
    /// request, over what its own settings give.
    pub prompt_cache_minutes: Option<u64>,
    /// The `[machine.proxy.<name>]` the tool's tracked agents reach their API through.
    pub proxy: Option<String>,
}

/// The `[machine.ai]` table as written and every AI tool resolved from it once,
/// when the config is read.
#[derive(Debug, Deserialize)]
#[serde(from = "Agents")]
pub struct Table {
    /// The entries as written, which validation reads.
    pub entries: Agents,
    pub tools: Tools,
}

impl From<Agents> for Table {
    fn from(entries: Agents) -> Self {
        Self {
            tools: resolve::tools(&entries),
            entries,
        }
    }
}

impl Default for Table {
    fn default() -> Self {
        Agents::new().into()
    }
}

/// Launchers configured under `[machine.ai]`, which join the global `[commands]`.
pub fn commands(agents: &Agents) -> impl Iterator<Item = (String, Vec<String>)> + '_ {
    agents
        .iter()
        .filter_map(|(name, agent)| Some((name.clone(), agent.command.clone()?)))
}

/// The names of the tools that run as agents.
pub fn agents(tools: &Tools) -> impl Iterator<Item = &str> {
    tools
        .values()
        .filter(|tool| tool.runs_as_agent)
        .map(|tool| tool.name.as_str())
}

/// Each tool's user-level skill directory for a launch from this process;
/// `None` when it has none.
pub fn skill_dirs(tools: &Tools, home: &Path) -> Result<BTreeMap<String, Option<PathBuf>>> {
    tools
        .iter()
        .map(|(name, tool)| Ok((name.clone(), tool.skill_dir(home)?)))
        .collect()
}

/// Skill delivery reads machine config without constructing daemon paths.
pub fn load(home: &Path) -> Result<Tools> {
    let path = crate::config::Config::path_for_home(home);
    let Some(text) =
        crate::fsutil::read_optional(&path).with_context(|| format!("read {}", path.display()))?
    else {
        return Ok(Table::default().tools);
    };
    let config = crate::config::Config::from_toml(&text)
        .with_context(|| format!("parse {}", path.display()))?;
    validate(
        &config.machine.ai.entries,
        &config.repository.commands,
        home,
    )?;
    Ok(config.machine.ai.tools)
}

#[cfg(test)]
mod tests;
