use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::{fs, path::PathBuf, time::Duration};

use crate::{paths::Paths, repo_config::RepoConfig};

/// The global file's repository-overridable scalars as written, so an omitted
/// one stays distinguishable from its built-in default. Unknown fields are
/// the full parse's concern, so global-only settings never break this view.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RepositoryPresence {
    codex: CodexPresence,
    auto_cleanup: AutoCleanupPresence,
    pr_cleanup: PrCleanupPresence,
    ports: PortsPresence,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CodexPresence {
    default_mode: Option<crate::cli::CodexMode>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct AutoCleanupPresence {
    enabled: Option<bool>,
    idle_minutes: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PrCleanupPresence {
    enabled: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PortsPresence {
    start: Option<u16>,
    end: Option<u16>,
}

/// Settings a repository may set, after every layer: a repository value
/// wins, an omitted one keeps the global value or the built-in default.
#[derive(Debug)]
pub struct Effective {
    pub commands: crate::named_commands::Commands,
    pub issue_template: Option<String>,
    pub agent_template: Option<String>,
    pub agent_auth: crate::agent_auth::Config,
    pub default_agent: Option<crate::cli::Agent>,
    pub codex: Codex,
    pub auto_cleanup: AutoCleanup,
    pub pr_cleanup: PrCleanup,
    pub ports: Ports,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub commands: crate::named_commands::Commands,
    pub issue_template: Option<String>,
    pub agent_template: Option<String>,
    pub agent_auth: crate::agent_auth::Config,
    pub git: crate::git_profile::Git,
    pub git_profile: Option<String>,
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

impl Ports {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.start > 0 && self.start <= self.end,
            "ports.start/end must specify a nonempty range between 1 and 65535"
        );
        Ok(())
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
    fn template_states_the_defaults_and_its_examples_are_valid() {
        let paths = Paths {
            home: "/home/test".into(),
            state: "/separate/state".into(),
            socket: "/separate/state/daemon.sock".into(),
        };
        assert!(PACKAGED.contains(&("default", TEMPLATE)));
        for (name, text) in PACKAGED {
            Config::parse(text, &paths)
                .unwrap_or_else(|error| panic!("invalid packaged config {name}: {error:#}"));
        }
        let written = Config::parse(TEMPLATE, &paths).unwrap();
        let defaults = Config::default();
        assert_eq!(
            written.root_dir(&paths).unwrap(),
            defaults.root_dir(&paths).unwrap()
        );
        assert_eq!(written.default_agent, None);
        assert_eq!(written.codex.default_mode, defaults.codex.default_mode);
        assert_eq!(written.auto_cleanup.enabled, defaults.auto_cleanup.enabled);
        assert_eq!(
            written.auto_cleanup.idle_minutes,
            defaults.auto_cleanup.idle_minutes
        );
        assert_eq!(written.pr_cleanup.enabled, defaults.pr_cleanup.enabled);
        assert_eq!(
            (written.ports.start, written.ports.end),
            (defaults.ports.start, defaults.ports.end)
        );
        assert_eq!(
            (
                written.simulators.max_booted,
                written.simulators.max_devices,
                written.simulators.idle_seconds,
                written.simulators.allow_any
            ),
            (
                defaults.simulators.max_booted,
                defaults.simulators.max_devices,
                defaults.simulators.idle_seconds,
                defaults.simulators.allow_any
            )
        );
        assert!(written.simulators.default.is_none() && written.simulators.profiles.is_empty());
        assert!(written.resources.is_empty() && written.resource_pools.is_empty());
        // Every commented example, uncommented, is a valid setting.
        let enabled: String = TEMPLATE
            .lines()
            .map(|line| match line.strip_prefix("# ") {
                Some(setting) if setting.starts_with('[') || setting.contains(" = ") => {
                    format!("{setting}\n")
                }
                _ => format!("{line}\n"),
            })
            .collect();
        let config = Config::parse(&enabled, &paths).unwrap();
        assert_eq!(config.default_agent, Some(crate::cli::Agent::Codex));
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
        // Reset keeps the edited file as a backup and restores the template.
        let (same, backup) = Config::replace_at(expected.clone(), default_template()).unwrap();
        let backup = backup.unwrap();
        assert!(same == expected && backup == home.path().join(".config/shoal/config.toml.backup"));
        assert_eq!(fs::read_to_string(&backup).unwrap(), text);
        assert_eq!(fs::read_to_string(&path).unwrap(), TEMPLATE);
        fs::remove_file(&path).unwrap();
        assert_eq!(
            Config::replace_at(expected.clone(), default_template())
                .unwrap()
                .1,
            None
        );
        assert_eq!(fs::read_to_string(&backup).unwrap(), text);
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
    fn repository_values_win_over_global_cleanup_policy() {
        let global: Config =
            toml::from_str("[auto_cleanup]\nenabled = false\nidle_minutes = 30\n").unwrap();
        let repo = crate::repo_config::parse(
            "[auto_cleanup]\nenabled = true\n[pr_cleanup]\nenabled = false\n",
        )
        .unwrap();
        let effective = global.effective(&repo).unwrap();
        assert_eq!(
            effective.auto_cleanup.delay(),
            Some(Duration::from_secs(1800))
        );
        assert!(!effective.pr_cleanup.enabled);
        let repo = crate::repo_config::parse("[auto_cleanup]\nidle_minutes = 5\n").unwrap();
        let effective = global.effective(&repo).unwrap();
        assert_eq!(effective.auto_cleanup.delay(), None);
        assert_eq!(effective.auto_cleanup.idle_minutes, 5);
        assert!(effective.pr_cleanup.enabled);
        assert_eq!(
            Config::default()
                .effective(&RepoConfig::default())
                .unwrap()
                .auto_cleanup
                .delay(),
            Some(Duration::from_secs(600))
        );
    }

    #[test]
    fn agent_defaults_come_from_the_repository_before_the_global_config() {
        use crate::cli::{Agent, CodexMode};

        let global: Config =
            toml::from_str("default_agent = 'claude'\n[codex]\ndefault_mode = 'app'\n").unwrap();
        let repo = crate::repo_config::parse("default_agent = 'codex'\n").unwrap();
        let effective = global.effective(&repo).unwrap();
        assert_eq!(effective.default_agent, Some(Agent::Codex));
        assert_eq!(effective.codex.default_mode, CodexMode::App);
        let repo = crate::repo_config::parse("[codex]\ndefault_mode = 'cli'\n").unwrap();
        let effective = global.effective(&repo).unwrap();
        assert_eq!(effective.default_agent, Some(Agent::Claude));
        assert_eq!(effective.codex.default_mode, CodexMode::Cli);
        let effective = Config::default().effective(&repo).unwrap();
        assert_eq!(effective.default_agent, None);
    }

    #[test]
    fn port_range_layers_per_bound_and_must_stay_nonempty() {
        let global: Config = toml::from_str("[ports]\nstart = 3000\nend = 3100\n").unwrap();
        let repo =
            crate::repo_config::parse("[ports]\nstart = 3050\n[ports.web]\nport = 8080\n").unwrap();
        let ports = global.effective(&repo).unwrap().ports;
        assert_eq!((ports.start, ports.end), (3050, 3100));
        let ports = global.effective(&RepoConfig::default()).unwrap().ports;
        assert_eq!((ports.start, ports.end), (3000, 3100));
        let repo = crate::repo_config::parse("[ports]\nend = 2000\n").unwrap();
        assert!(global.effective(&repo).is_err());
        for text in ["[ports]\nstart = 0\n", "[ports]\nstart = 5\nend = 4\n"] {
            assert!(crate::repo_config::parse(text).is_err(), "{text}");
        }
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

impl AutoCleanup {
    /// The idle delay before removal; `None` when disabled.
    pub fn delay(&self) -> Option<Duration> {
        self.enabled
            .then(|| Duration::from_secs(self.idle_minutes * 60))
    }
}

pub fn validate_idle_minutes(minutes: u64) -> Result<()> {
    ensure!(
        minutes > 0 && minutes <= 525600,
        "auto_cleanup.idle_minutes must be between 1 and 525600"
    );
    Ok(())
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

    /// Write a fresh template, first moving any existing file to
    /// `config.toml.backup` (replacing an older backup). Returns the path and
    /// the backup, if one was made.
    pub fn reset(paths: &Paths) -> Result<(PathBuf, Option<PathBuf>)> {
        Self::install_named(paths, "default")
    }

    pub fn install_named(paths: &Paths, name: &str) -> Result<(PathBuf, Option<PathBuf>)> {
        let text = PACKAGED
            .iter()
            .find(|(candidate, _)| *candidate == name)
            .map(|(_, text)| *text)
            .with_context(|| format!("unknown packaged config: {name}"))?;
        Self::parse(text, paths).with_context(|| format!("validate packaged config {name}"))?;
        Self::replace_at(Self::path(paths), text)
    }

    fn replace_at(path: PathBuf, text: &str) -> Result<(PathBuf, Option<PathBuf>)> {
        let parent = path.parent().context("config path has no parent")?;
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        // Prepare the complete replacement before moving the user's current file.
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        std::io::Write::write_all(&mut temporary, text.as_bytes())?;
        if let Ok(metadata) = fs::symlink_metadata(&path) {
            ensure!(
                !metadata.is_dir(),
                "config is a directory: {}",
                path.display()
            );
        }
        let backup = path.with_extension("toml.backup");
        let moved = match fs::rename(&path, &backup) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("move {} to {}", path.display(), backup.display()));
            }
        };
        temporary
            .persist_noclobber(&path)
            .with_context(|| format!("install config at {}", path.display()))?;
        Ok((path, moved.then_some(backup)))
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
                std::io::Write::write_all(&mut file, default_template().as_bytes())
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

    /// Read the global config and project its repository-overridable values
    /// into a layer whose omitted scalar fields remain distinguishable from
    /// built-in defaults. The file is parsed twice: once fully, once for
    /// presence.
    pub fn load_with_repository_layer(paths: &Paths) -> Result<(Self, RepoConfig)> {
        let path = Self::path(paths);
        let config = Self::load(paths)?;
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        };
        let presence: RepositoryPresence =
            toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
        let directory = path.parent().context("config has no directory")?;
        let repository_layer = RepoConfig {
            commands: config.commands.clone(),
            issue_template: config.issue_template.clone().or(crate::templates::read(
                directory,
                crate::templates::ISSUE_FILE,
            )?),
            agent_template: config.agent_template.clone().or(crate::templates::read(
                directory,
                crate::templates::AGENT_FILE,
            )?),
            agent_auth: config.agent_auth.clone(),
            git_profile: config.git_profile.clone(),
            default_agent: config.default_agent,
            codex: crate::repo_config::Codex {
                default_mode: presence.codex.default_mode,
            },
            ports: crate::repo_config::PortDefaults {
                start: presence.ports.start,
                end: presence.ports.end,
                ..Default::default()
            },
            resources: config.resources.clone(),
            resource_pools: config.resource_pools.clone(),
            auto_cleanup: crate::repo_config::AutoCleanup {
                enabled: presence.auto_cleanup.enabled,
                idle_minutes: presence.auto_cleanup.idle_minutes,
            },
            pr_cleanup: crate::repo_config::PrCleanup {
                enabled: presence.pr_cleanup.enabled,
            },
            ..Default::default()
        };
        Ok((config, repository_layer))
    }

    fn parse(text: &str, paths: &Paths) -> Result<Self> {
        let config: Self = toml::from_str(text)?;
        config.root_dir(paths)?;
        validate_idle_minutes(config.auto_cleanup.idle_minutes)?;
        config.ports.validate()?;
        config.simulators.validate()?;
        config.git.validate()?;
        config.agent_auth.validate()?;
        crate::named_commands::validate(&config.commands)?;
        if let Some(name) = &config.git_profile {
            config.git.profile(name)?;
        }
        crate::resources::definitions(&config.resources, &config.resource_pools)?;
        Ok(config)
    }

    /// Fails when the layers combine into an invalid setting, such as a
    /// repository `ports.start` above the global `ports.end`.
    pub fn effective(&self, repo: &RepoConfig) -> Result<Effective> {
        let ports = Ports {
            start: repo.ports.start.unwrap_or(self.ports.start),
            end: repo.ports.end.unwrap_or(self.ports.end),
        };
        ports.validate()?;
        let mut commands = crate::named_commands::defaults();
        commands.extend(self.commands.clone());
        commands.extend(repo.commands.clone());
        Ok(Effective {
            commands,
            issue_template: repo
                .issue_template
                .clone()
                .or_else(|| self.issue_template.clone()),
            agent_template: repo
                .agent_template
                .clone()
                .or_else(|| self.agent_template.clone()),
            agent_auth: repo.agent_auth.clone().over(self.agent_auth.clone()),
            default_agent: repo.default_agent.or(self.default_agent),
            codex: Codex {
                default_mode: repo.codex.default_mode.unwrap_or(self.codex.default_mode),
            },
            auto_cleanup: AutoCleanup {
                enabled: repo
                    .auto_cleanup
                    .enabled
                    .unwrap_or(self.auto_cleanup.enabled),
                idle_minutes: repo
                    .auto_cleanup
                    .idle_minutes
                    .unwrap_or(self.auto_cleanup.idle_minutes),
            },
            pr_cleanup: PrCleanup {
                enabled: repo.pr_cleanup.enabled.unwrap_or(self.pr_cleanup.enabled),
            },
            ports,
        })
    }
}

/// Written by `shoal install` when no config exists: the defaults, stated.
#[cfg(test)]
const TEMPLATE: &str = include_str!("../configs/default.toml");

fn default_template() -> &'static str {
    PACKAGED
        .iter()
        .find(|(name, _)| *name == "default")
        .expect("packaged default config")
        .1
}

/// Named templates shipped in this binary, independent of the source checkout.
pub const PACKAGED: &[(&str, &str)] = include!(concat!(env!("OUT_DIR"), "/packaged_configs.rs"));

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
