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
    /// Agent `shoal issue` starts when `--agent` is omitted.
    pub default_agent: Option<crate::cli::Agent>,
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
    fn template_is_inert_and_every_commented_setting_is_valid() {
        let paths = Paths {
            home: "/home/test".into(),
            state: "/separate/state".into(),
            socket: "/separate/state/daemon.sock".into(),
        };
        let inert = Config::parse(TEMPLATE, &paths).unwrap();
        assert!(inert.default_agent.is_none() && inert.simulators.profiles.is_empty());
        let enabled: String = TEMPLATE
            .lines()
            .filter(|line| !line.starts_with("##"))
            .map(|line| format!("{}\n", line.strip_prefix("# ").unwrap_or(line)))
            .collect();
        let config = Config::parse(&enabled, &paths).unwrap();
        assert_eq!(config.default_agent, Some(crate::cli::Agent::Codex));
        assert_eq!(config.auto_cleanup.idle_minutes, 10);
        assert_eq!(config.ports.start, 49152);
        assert!(config.simulators.profiles.contains_key("phone"));
        assert!(config.resource_pools.contains_key("devices"));
    }

    #[test]
    fn install_writes_the_template_once_and_keeps_edits() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths {
            home: home.path().to_owned(),
            state: home.path().join("state"),
            socket: home.path().join("state/daemon.sock"),
        };
        let expected = home.path().join(".config/shoal/config.toml");
        let (path, created) = Config::install_at(expected.clone()).unwrap();
        assert!(created && path == expected);
        assert_eq!(fs::read_to_string(&path).unwrap(), TEMPLATE);
        fs::write(&path, "default_agent = 'claude'\n").unwrap();
        let (same, created) = Config::install_at(expected.clone()).unwrap();
        assert!(!created && same == expected);
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(
            Config::parse(&text, &paths).unwrap().default_agent,
            Some(crate::cli::Agent::Claude)
        );
    }

    #[test]
    fn default_agent_accepts_agent_spellings_only() {
        use crate::{cli::Agent, happy::HappyAgent};

        assert_eq!(toml::from_str::<Config>("").unwrap().default_agent, None);
        for (text, agent) in [
            ("default_agent = 'codex'", Agent::Codex),
            ("default_agent = 'claude'", Agent::Claude),
            (
                "default_agent = 'happy-codex'",
                Agent::Happy(HappyAgent::Codex),
            ),
        ] {
            let config: Config = toml::from_str(text).unwrap();
            assert_eq!(config.default_agent, Some(agent));
        }
        let error = toml::from_str::<Config>("default_agent = 'happy'")
            .unwrap_err()
            .to_string();
        assert!(error.contains("happy-claude"), "{error}");
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

    /// `$XDG_CONFIG_HOME/shoal/config.toml`, or `~/.config/shoal/config.toml`.
    pub fn path(paths: &Paths) -> PathBuf {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| paths.home.join(".config"))
            .join("shoal/config.toml")
    }

    /// Write the commented template unless a config file already exists.
    /// Returns the path and whether this call created it.
    pub fn install(paths: &Paths) -> Result<(PathBuf, bool)> {
        Self::install_at(Self::path(paths))
    }

    fn install_at(path: PathBuf) -> Result<(PathBuf, bool)> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                std::io::Write::write_all(&mut file, TEMPLATE.as_bytes())
                    .with_context(|| format!("write {}", path.display()))?;
                Ok((path, true))
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok((path, false)),
            Err(error) => Err(error).with_context(|| format!("create {}", path.display())),
        }
    }

    pub fn load(paths: &Paths) -> Result<Self> {
        let path = Self::path(paths);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        };
        Self::parse(&text, paths).with_context(|| format!("parse {}", path.display()))
    }

    fn parse(text: &str, paths: &Paths) -> Result<Self> {
        let config: Self = toml::from_str(text)?;
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

/// Written by `shoal setup` when no config exists. Settings are commented out
/// at their defaults, examples are marked as such, so the file changes nothing
/// until edited.
const TEMPLATE: &str = r##"## Shoal machine configuration. Settings are read per command; restart the
## daemon (`shoal daemon restart`) after changing cleanup or port ranges.
## Settings are commented out at their defaults: uncomment a line to change it.
## Blocks marked as examples are not defaults; uncomment a whole block and adapt it.
## Repository settings (named ports, setup and hook commands, resource pools)
## live in each repository's .shoal.toml.

## Parent of every repository's workspaces and URL clones.
# root_dir = "~/shoal"

## Agent `shoal issue <url>` starts when --agent is omitted:
## codex, claude, happy-claude, or happy-codex.
# default_agent = "codex"

# [codex]
## `shoal codex` without cli/app: "cli" or "app".
# default_mode = "cli"

# [auto_cleanup]
## Remove idle, clean, pushed or landed workspaces automatically.
# enabled = true
# idle_minutes = 10

# [pr_cleanup]
## Remove workspaces whose watched PR has merged.
# enabled = true

# [ports]
## Range for automatic TCP port reservations.
# start = 49152
# end = 65535

# [simulators]
## Xcode simulator leases (macOS).
# max_booted = 2
# max_devices = 4
# idle_seconds = 120
# allow_any = false
## Example profile; `default` must name a profile defined below it.
# default = "phone"
# [simulators.profiles.phone]
# device = "iPhone 17"
# runtime = "iOS 26"

## Example permits shared across repositories; see the command reference.
# [resources.signing]
# capacity = 1
# reason = "Signing service"
# [resource_pools.devices]
# capacity = 2
# [resource_pools.devices.resources.alpha]
# capacity = 1
"##;

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
