use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::{fs, path::PathBuf};

use crate::paths::Paths;

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub auto_cleanup: AutoCleanup,
    pub ports: Ports,
    pub resources: std::collections::BTreeMap<String, crate::resources::ResourceConfig>,
    pub resource_pools: std::collections::BTreeMap<String, crate::resources::PoolConfig>,
    pub simulators: crate::simulators::SimConfig,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Ports {
    pub start: u16,
    pub end: u16,
}

impl Default for Ports {
    fn default() -> Self {
        Self {
            start: 49152,
            end: 65535,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_defaults_can_be_disabled_and_typos_are_rejected() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.auto_cleanup.enabled);
        assert_eq!(config.auto_cleanup.idle_minutes, 10);
        let config: Config =
            toml::from_str("[auto_cleanup]\nenabled = false\nidle_minutes = 30\n").unwrap();
        assert!(!config.auto_cleanup.enabled);
        assert_eq!(config.auto_cleanup.idle_minutes, 30);
        assert!(toml::from_str::<Config>("[auto_cleanpu]\nenabled = false").is_err());
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AutoCleanup {
    pub enabled: bool,
    pub idle_minutes: u64,
}

impl Default for AutoCleanup {
    fn default() -> Self {
        Self {
            enabled: true,
            idle_minutes: 10,
        }
    }
}

impl Config {
    pub fn load(paths: &Paths) -> Result<Self> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| paths.home.join(".config"));
        let path = base.join("shoal/config.toml");
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        };
        let config: Self =
            toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
        ensure!(
            config.auto_cleanup.idle_minutes > 0 && config.auto_cleanup.idle_minutes <= 525600,
            "auto_cleanup.idle_minutes must be between 1 and 525600"
        );
        ensure!(
            config.ports.start > 0 && config.ports.start <= config.ports.end,
            "ports.start/end must specify a nonempty range between 1 and 65535"
        );
        config.simulators.validate()?;
        crate::resources::definitions(&config.resources, &config.resource_pools)?;
        Ok(config)
    }
}
