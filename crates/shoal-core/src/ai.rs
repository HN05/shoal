//! AI tools: the providers Shoal ships and the user's `[agents.<name>]`
//! additions, each resolved into a [`Tool`].
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::Result;

mod builtin;
mod launch;
mod resolve;
pub(crate) mod validate;

pub use builtin::PROVIDERS;
pub(crate) use builtin::default_commands;
pub(crate) use launch::Arguments;
pub use launch::{Environment, ReviewFields, Schema, oneshot_command};
pub use resolve::{DEFAULT_PROMPT_CACHE, Tool, tools};
pub(crate) use resolve::{LaunchProfile, Tools, inherit_launchers};
pub use shoal_protocol::integrations::IntegrationKind;
pub(crate) use validate::validate;

pub(crate) type Agents = BTreeMap<String, Agent>;

/// One AI tool as its settings and this machine describe it.
#[derive(Debug, Default)]
pub struct Agent {
    pub(crate) skill_dir: Option<PathBuf>,
    pub(crate) command: Option<Vec<String>>,
    pub(crate) instructions: Option<Vec<String>>,
    pub(crate) resume: Option<Vec<String>>,
    pub(crate) resume_latest: Option<Vec<String>>,
    pub(crate) fork: Option<Vec<String>>,
    pub(crate) add_dir: Option<Vec<String>>,
    pub(crate) model: Option<String>,
    pub(crate) effort: Option<String>,
    pub(crate) model_args: Option<Vec<String>>,
    pub(crate) effort_args: Option<Vec<String>>,
    pub(crate) review: Option<Vec<String>>,
    pub(crate) review_resume: Option<Vec<String>>,
    pub(crate) gate: Option<Vec<String>>,
    pub(crate) gate_resume: Option<Vec<String>>,
    pub(crate) base: Option<String>,
    pub(crate) env: BTreeMap<String, String>,
    pub(crate) env_remove: Vec<String>,
    pub(crate) retry: Option<bool>,
    pub(crate) prompt_cache_minutes: Option<u64>,
    pub(crate) proxy: Option<String>,
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

/// The AI tools the global config under `home` describes, read without the
/// daemon's paths.
pub fn load(home: &Path) -> Result<Tools> {
    let config = crate::config::Config::load_unchecked(home)?;
    validate::skill_dirs(&config.machine.agents, home)?;
    config.tools()
}

#[cfg(test)]
mod tests;
