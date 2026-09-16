use anyhow::{Context, Result, ensure};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, path::Path};

#[derive(Debug, Serialize, Deserialize)]
pub struct LocalConfig {
    pub repository_id: String,
    pub toml: Option<String>,
}

#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum ConflictPolicy {
    Auto,
    #[default]
    Suggest,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RepoConfig {
    pub ports: PortDefaults,
    pub resources: BTreeMap<String, crate::resources::ResourceConfig>,
    pub resource_pools: BTreeMap<String, crate::resources::PoolConfig>,
    pub simulators: SimulatorPreferences,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct PortDefaults {
    pub on_conflict: ConflictPolicy,
    #[serde(flatten)]
    pub definitions: BTreeMap<String, PortDefinition>,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PortDefinition {
    pub port: Option<u16>,
    pub env: Option<String>,
    pub reason: Option<String>,
    pub on_conflict: Option<ConflictPolicy>,
}

pub fn load(workspace_dir: &Path) -> Result<RepoConfig> {
    let paths = [
        workspace_dir.join(".shoal.toml"),
        workspace_dir.join(".shoal/config.toml"),
    ];
    let found: Vec<_> = paths.iter().filter(|p| p.exists()).collect();
    ensure!(
        found.len() <= 1,
        "both .shoal.toml and .shoal/config.toml exist; keep only one repository config"
    );
    let Some(path) = found.first() else {
        return Ok(RepoConfig::default());
    };
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    parse(&text).with_context(|| format!("parse {}", path.display()))
}

pub fn parse(text: &str) -> Result<RepoConfig> {
    let config: RepoConfig = toml::from_str(text)?;
    for (name, definition) in &config.ports.definitions {
        ensure!(
            !name.is_empty()
                && name.len() <= 64
                && name.as_bytes()[0].is_ascii_lowercase()
                && name.bytes().all(|c| c.is_ascii_lowercase()
                    || c.is_ascii_digit()
                    || c == b'_'
                    || c == b'-'),
            "invalid configured port name: {name}"
        );
        ensure!(
            definition.port != Some(0),
            "configured port {name} cannot use port zero"
        );
    }
    crate::resources::definitions(&config.resources, &config.resource_pools)?;
    Ok(config)
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SimulatorPreferences {
    pub preferred: Vec<String>,
}
