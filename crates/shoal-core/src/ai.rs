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
pub mod validate;

pub use builtin::{PROVIDERS, default_commands};
pub use launch::{Arguments, Environment, ReviewFields, Schema, oneshot_command};
pub use resolve::{DEFAULT_PROMPT_CACHE, LaunchProfile, Tool, Tools, inherit_launchers, tools};
pub use shoal_protocol::integrations::IntegrationKind;
pub use validate::validate;

pub type Agents = BTreeMap<String, Agent>;

/// One AI tool as its settings and this machine describe it.
#[derive(Debug, Default)]
pub struct Agent {
    pub skill_dir: Option<PathBuf>,
    pub command: Option<Vec<String>>,
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
    pub base: Option<String>,
    pub env: BTreeMap<String, String>,
    pub env_remove: Vec<String>,
    pub retry: Option<bool>,
    pub prompt_cache_minutes: Option<u64>,
    pub proxy: Option<String>,
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
