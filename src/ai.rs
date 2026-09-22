//! Machine-local settings for user-configured AI tools.
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

pub type Agents = BTreeMap<String, Agent>;
pub const BUILT_INS: [&str; 2] = ["codex", "claude"];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Agent {
    /// Parent directory in which Shoal installs `shoal/SKILL.md`.
    pub skill_dir: PathBuf,
}

pub fn validate(agents: &Agents, home: &Path) -> Result<()> {
    for (name, agent) in agents {
        crate::validate::name("AI tool", name)?;
        ensure!(
            name != "all",
            "AI tool name 'all' is reserved for skill installation"
        );
        skill_dir(agent, home).with_context(|| format!("ai.{name}.skill_dir"))?;
    }
    Ok(())
}

pub fn skill_dir(agent: &Agent, home: &Path) -> Result<PathBuf> {
    let path = match agent.skill_dir.strip_prefix("~") {
        Ok(relative) => home.join(relative),
        Err(_) => agent.skill_dir.clone(),
    };
    ensure!(
        path.is_absolute(),
        "skill_dir must be an absolute path or start with ~/"
    );
    ensure!(
        !path.as_os_str().as_encoded_bytes().contains(&0),
        "skill_dir must not contain NUL"
    );
    Ok(path)
}

/// Skill delivery reads machine config without constructing daemon paths.
pub fn load(home: &Path) -> Result<Agents> {
    let path = crate::config::Config::path_for_home(home);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Agents::new()),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let config: crate::config::Config =
        toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    validate(&config.ai, home)?;
    Ok(config.ai)
}
