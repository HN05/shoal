use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::{fs, path::PathBuf};

use crate::paths::Paths;

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub root_dir: Option<PathBuf>,
    /// Former name of `root_dir`; still accepted so existing configs load.
    pub repositories_dir: Option<PathBuf>,
    pub codex: Codex,
    pub auto_cleanup: AutoCleanup,
    pub pr_cleanup: PrCleanup,
    pub ports: Ports,
    pub resources: std::collections::BTreeMap<String, crate::resources::ResourceConfig>,
    pub resource_pools: std::collections::BTreeMap<String, crate::resources::PoolConfig>,
    pub simulators: crate::simulators::SimConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Codex {
    pub default_mode: crate::cli::CodexMode,
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
    fn codex_mode_defaults_to_cli_and_rejects_invalid_settings() {
        use crate::cli::CodexMode;

        for text in ["", "[codex]", "[codex]\ndefault_mode = 'cli'"] {
            let config: Config = toml::from_str(text).unwrap();
            assert_eq!(config.codex.default_mode, CodexMode::Cli);
        }
        let config: Config = toml::from_str("[codex]\ndefault_mode = 'app'").unwrap();
        assert_eq!(config.codex.default_mode, CodexMode::App);
        for text in [
            "[codex]\ndefault_mode = 'desktop'",
            "[codex]\ndefaut_mode = 'app'",
        ] {
            assert!(toml::from_str::<Config>(text).is_err());
        }
    }

    #[test]
    fn cleanup_defaults_can_be_disabled_and_typos_are_rejected() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.auto_cleanup.enabled);
        assert!(config.pr_cleanup.enabled);
        assert!(
            !toml::from_str::<Config>("[pr_cleanup]\nenabled=false")
                .unwrap()
                .pr_cleanup
                .enabled
        );
        assert!(toml::from_str::<Config>("[pr_cleanup]\nenabld=false").is_err());
        assert_eq!(config.auto_cleanup.idle_minutes, 10);
        let config: Config =
            toml::from_str("[auto_cleanup]\nenabled = false\nidle_minutes = 30\n").unwrap();
        assert!(!config.auto_cleanup.enabled);
        assert_eq!(config.auto_cleanup.idle_minutes, 30);
        assert!(toml::from_str::<Config>("[auto_cleanpu]\nenabled = false").is_err());
    }

    #[test]
    fn root_directory_defaults_and_validates_explicit_paths() {
        let paths = Paths {
            home: "/home/test".into(),
            state: "/separate/state".into(),
            socket: "/separate/state/daemon.sock".into(),
        };
        assert_eq!(
            Config::default().root_dir(&paths).unwrap(),
            PathBuf::from("/home/test/shoal")
        );
        for (value, expected) in [
            ("~/Projects/repos", "/home/test/Projects/repos"),
            ("/external/repos", "/external/repos"),
        ] {
            let config: Config = toml::from_str(&format!("root_dir = {value:?}")).unwrap();
            assert_eq!(config.root_dir(&paths).unwrap(), PathBuf::from(expected));
        }
        for value in ["", "relative/repos", "~someone/repos"] {
            let config: Config = toml::from_str(&format!("root_dir = {value:?}")).unwrap();
            assert!(config.root_dir(&paths).is_err());
        }
        let config: Config = toml::from_str("repositories_dir = \"~/old\"").unwrap();
        assert_eq!(
            config.root_dir(&paths).unwrap(),
            PathBuf::from("/home/test/old")
        );
        let config: Config =
            toml::from_str("repositories_dir = \"~/old\"\nroot_dir = \"~/new\"").unwrap();
        assert_eq!(
            config.root_dir(&paths).unwrap(),
            PathBuf::from("/home/test/new")
        );
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
    /// Parent of every repository directory: `<root>/<repo>/` holds the
    /// repository's URL clone as `.checkout` and its workspaces as siblings.
    pub fn root_dir(&self, paths: &Paths) -> Result<PathBuf> {
        let Some(path) = self.root_dir.as_ref().or(self.repositories_dir.as_ref()) else {
            return Ok(paths.home.join("shoal"));
        };
        let path = match path.strip_prefix("~") {
            Ok(relative) => paths.home.join(relative),
            Err(_) => path.clone(),
        };
        ensure!(
            path.is_absolute(),
            "root_dir must be an absolute path or start with ~/"
        );
        Ok(path)
    }

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
        config.root_dir(paths)?;
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

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PrCleanup {
    pub enabled: bool,
}
impl Default for PrCleanup {
    fn default() -> Self {
        Self { enabled: true }
    }
}
