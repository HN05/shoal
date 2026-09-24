pub mod edit;
pub mod named_commands;
mod placeholders;
pub mod repo;
pub mod report;
pub mod resolve;
pub mod templates;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf, time::Duration};

use crate::paths::Paths;
use repo::{ConfigLayers, RepoConfig};
pub use resolve::Effective;

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub ai: crate::ai::Agents,
    pub commands: named_commands::Commands,
    pub issue_template: Option<String>,
    pub agent_template: Option<String>,
    pub agent_auth: crate::agent_auth::Config,
    pub git: crate::git_profile::Git,
    pub git_profile: Option<String>,
    pub pre_setup_cmd: Option<String>,
    pub post_remove_cmd: Option<String>,
    pub post_resource_acquire_cmd: Option<String>,
    pub pre_resource_release_cmd: Option<String>,
    pub root_dir: Option<PathBuf>,
    /// Former name of `root_dir`; still accepted so existing configs load.
    pub repositories_dir: Option<PathBuf>,
    /// Agent `shoal issue` starts when `--agent` is omitted.
    pub default_agent: Option<crate::cli::Agent>,
    pub codex: repo::Codex,
    pub auto_cleanup: repo::AutoCleanup,
    pub pr_cleanup: repo::PrCleanup,
    pub ports: PortRange,
    pub resources: std::collections::BTreeMap<String, crate::daemon::resources::ResourceConfig>,
    pub resource_pools: std::collections::BTreeMap<String, crate::daemon::resources::PoolConfig>,
    pub simulators: crate::sim::SimConfig,
}

#[derive(Debug, Default, Serialize)]
pub struct Codex {
    pub default_mode: crate::cli::CodexMode,
}

/// The global `[ports]` range as written, so an omitted bound stays
/// distinguishable from its built-in default.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PortRange {
    pub start: Option<u16>,
    pub end: Option<u16>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Ports {
    pub start: u16,
    pub end: u16,
    pub on_conflict: repo::ConflictPolicy,
    #[serde(flatten)]
    pub definitions: std::collections::BTreeMap<String, repo::PortDefinition>,
}

impl Default for Ports {
    fn default() -> Self {
        Self {
            start: 49152,
            end: 65535,
            on_conflict: repo::ConflictPolicy::default(),
            definitions: Default::default(),
        }
    }
}

/// The repository-overridable simulator settings after every layer.
#[derive(Debug, Default, Clone, Serialize)]
pub struct Simulators {
    pub requires_approval: bool,
    pub approval_lifetime: crate::daemon::access::Lifetime,
    pub preferred: Vec<String>,
}

impl Ports {
    pub fn validate(&self) -> Result<()> {
        validate_port_range(Some(self.start), Some(self.end))
    }
}

/// The bounds one layer states; a range split across layers is checked
/// again once they combine.
pub fn validate_port_range(start: Option<u16>, end: Option<u16>) -> Result<()> {
    ensure!(start != Some(0), "ports.start must be between 1 and 65535");
    if let (Some(start), Some(end)) = (start, end) {
        ensure!(
            start <= end,
            "ports.start/end must specify a nonempty range"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Config {
        /// The settings for a repository whose only layer is `repo`.
        fn effective(&self, repo: &RepoConfig) -> Result<Effective> {
            self.resolve(&ConfigLayers {
                worktree_file: repo.clone(),
                ..Default::default()
            })
        }
    }

    #[test]
    fn codex_mode_defaults_to_cli_and_rejects_invalid_settings() {
        use crate::cli::CodexMode;

        for text in ["", "[codex]"] {
            let config: Config = toml::from_str(text).unwrap();
            assert_eq!(config.codex.default_mode, None);
            let effective = config.effective(&RepoConfig::default()).unwrap();
            assert_eq!(effective.codex.default_mode, CodexMode::Cli);
        }
        let config: Config = toml::from_str("[codex]\ndefault_mode = 'app'").unwrap();
        assert_eq!(config.codex.default_mode, Some(CodexMode::App));
        for text in [
            "[codex]\ndefault_mode = 'desktop'",
            "[codex]\ndefaut_mode = 'app'",
        ] {
            assert!(toml::from_str::<Config>(text).is_err());
        }
    }

    #[test]
    fn additional_hook_paths_are_validated_in_global_config() {
        let paths = Paths::for_test("/home/test");
        for &kind in crate::hooks::HookKind::ALL {
            let key = kind.key();
            if !kind.allows_global() {
                assert!(Config::parse(&format!("{key} = 'scripts/hook'"), &paths).is_err());
                continue;
            }
            assert!(Config::parse(&format!("{key} = 'scripts/hook'"), &paths).is_ok());
            assert!(Config::parse(&format!("{key} = ' '"), &paths).is_err());
            assert!(Config::parse(&format!(r#"{key} = "a\u0000b""#), &paths).is_err());
        }
    }

    #[test]
    fn template_states_the_defaults_and_its_examples_are_valid() {
        let paths = Paths::for_test("/home/test");
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
        // The template writes the built-in defaults out explicitly.
        let written_effective = written.effective(&RepoConfig::default()).unwrap();
        let default_effective = defaults.effective(&RepoConfig::default()).unwrap();
        assert_eq!(
            written.codex.default_mode,
            Some(default_effective.codex.default_mode)
        );
        assert_eq!(
            written.auto_cleanup.enabled,
            Some(default_effective.auto_cleanup.enabled)
        );
        assert_eq!(
            written.auto_cleanup.idle_minutes,
            Some(default_effective.auto_cleanup.idle_minutes)
        );
        assert_eq!(
            written.pr_cleanup.enabled,
            Some(default_effective.pr_cleanup.enabled)
        );
        assert_eq!(
            (written.ports.start, written.ports.end),
            (
                Some(default_effective.ports.start),
                Some(default_effective.ports.end)
            )
        );
        assert_eq!(
            written_effective.auto_cleanup.delay(),
            default_effective.auto_cleanup.delay()
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
        let paths = Paths::for_test(home.path());
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
            ("default_agent = 'pi'", Agent::Custom("pi".into())),
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
        assert_eq!(config.auto_cleanup.enabled, None);
        let effective = config.effective(&RepoConfig::default()).unwrap();
        assert!(effective.auto_cleanup.enabled);
        assert!(effective.pr_cleanup.enabled);
        assert_eq!(effective.auto_cleanup.idle_minutes, 10);
        assert_eq!(
            toml::from_str::<Config>("[pr_cleanup]\nenabled=false")
                .unwrap()
                .pr_cleanup
                .enabled,
            Some(false)
        );
        assert!(toml::from_str::<Config>("[pr_cleanup]\nenabld=false").is_err());
        let config: Config =
            toml::from_str("[auto_cleanup]\nenabled = false\nidle_minutes = 30\n").unwrap();
        assert_eq!(config.auto_cleanup.enabled, Some(false));
        assert_eq!(config.auto_cleanup.idle_minutes, Some(30));
        assert!(toml::from_str::<Config>("[auto_cleanpu]\nenabled = false").is_err());
    }

    #[test]
    fn repository_values_win_over_global_cleanup_policy() {
        let global: Config =
            toml::from_str("[auto_cleanup]\nenabled = false\nidle_minutes = 30\n").unwrap();
        let repo =
            repo::parse("[auto_cleanup]\nenabled = true\n[pr_cleanup]\nenabled = false\n").unwrap();
        let effective = global.effective(&repo).unwrap();
        assert_eq!(
            effective.auto_cleanup.delay(),
            Some(Duration::from_secs(1800))
        );
        assert!(!effective.pr_cleanup.enabled);
        let repo = repo::parse("[auto_cleanup]\nidle_minutes = 5\n").unwrap();
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
        let repo = repo::parse("default_agent = 'codex'\n").unwrap();
        let effective = global.effective(&repo).unwrap();
        assert_eq!(effective.default_agent, Some(Agent::Codex));
        assert_eq!(effective.codex.default_mode, CodexMode::App);
        let repo = repo::parse("[codex]\ndefault_mode = 'cli'\n").unwrap();
        let effective = global.effective(&repo).unwrap();
        assert_eq!(effective.default_agent, Some(Agent::Claude));
        assert_eq!(effective.codex.default_mode, CodexMode::Cli);
        let effective = Config::default().effective(&repo).unwrap();
        assert_eq!(effective.default_agent, None);
    }

    #[test]
    fn port_range_layers_per_bound_and_must_stay_nonempty() {
        let global: Config = toml::from_str("[ports]\nstart = 3000\nend = 3100\n").unwrap();
        let repo = repo::parse("[ports]\nstart = 3050\n[ports.web]\nport = 8080\n").unwrap();
        let ports = global.effective(&repo).unwrap().ports;
        assert_eq!((ports.start, ports.end), (3050, 3100));
        let ports = global.effective(&RepoConfig::default()).unwrap().ports;
        assert_eq!((ports.start, ports.end), (3000, 3100));
        let repo = repo::parse("[ports]\nend = 2000\n").unwrap();
        assert!(global.effective(&repo).is_err());
        for text in ["[ports]\nstart = 0\n", "[ports]\nstart = 5\nend = 4\n"] {
            assert!(repo::parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn root_directory_defaults_and_validates_explicit_paths() {
        let paths = Paths::for_test("/home/test");
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

#[derive(Debug, Serialize)]
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
        Self::path_for_home(&paths.home)
    }

    pub fn path_for_home(home: &std::path::Path) -> PathBuf {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home.join(".config"))
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

    pub fn edit(
        paths: &Paths,
        key: &str,
        value: Option<&str>,
    ) -> Result<(PathBuf, Option<PathBuf>)> {
        let path = Self::path(paths);
        let _lock = Self::lock_file(&path)?;
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        };
        let edited = edit::edit(&text, key, value)?;
        Self::parse(&edited, paths).context("invalid edited config")?;
        Self::replace_at(path, &edited)
    }

    pub fn install_named(paths: &Paths, name: &str) -> Result<(PathBuf, Option<PathBuf>)> {
        let text = PACKAGED
            .iter()
            .find(|(candidate, _)| *candidate == name)
            .map(|(_, text)| *text)
            .with_context(|| format!("unknown packaged config: {name}"))?;
        Self::parse(text, paths).with_context(|| format!("validate packaged config {name}"))?;
        let path = Self::path(paths);
        let _lock = Self::lock_file(&path)?;
        Self::replace_at(path, text)
    }

    fn lock_file(path: &std::path::Path) -> Result<fs::File> {
        fs::create_dir_all(path.parent().context("config path has no parent")?)?;
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path.with_extension("toml.lock"))?;
        fs2::FileExt::lock_exclusive(&file)?;
        Ok(file)
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

    /// The global file as one snapshot, with the prompt-template files beside
    /// it standing in for omitted inline templates.
    pub fn load(paths: &Paths) -> Result<Self> {
        let path = Self::path(paths);
        let mut config = match fs::read_to_string(&path) {
            Ok(text) => {
                Self::parse(&text, paths).with_context(|| format!("parse {}", path.display()))?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        };
        let directory = path.parent().context("config has no directory")?;
        if config.issue_template.is_none() {
            config.issue_template = templates::read(directory, templates::ISSUE_FILE)?;
        }
        if config.agent_template.is_none() {
            config.agent_template = templates::read(directory, templates::AGENT_FILE)?;
        }
        Ok(config)
    }

    /// This machine's repository-overridable settings as the layer below the
    /// repository's own; machine-only settings stay out of it.
    pub fn repository_layer(&self) -> RepoConfig {
        RepoConfig {
            commands: self.commands.clone(),
            issue_template: self.issue_template.clone(),
            agent_template: self.agent_template.clone(),
            agent_auth: self.agent_auth.clone(),
            git_profile: self.git_profile.clone(),
            default_agent: self.default_agent.clone(),
            codex: self.codex,
            pre_setup_cmd: self.pre_setup_cmd.clone(),
            post_remove_cmd: self.post_remove_cmd.clone(),
            post_resource_acquire_cmd: self.post_resource_acquire_cmd.clone(),
            pre_resource_release_cmd: self.pre_resource_release_cmd.clone(),
            ports: repo::PortDefaults {
                start: self.ports.start,
                end: self.ports.end,
                ..Default::default()
            },
            resources: self.resources.clone(),
            resource_pools: self.resource_pools.clone(),
            simulators: repo::SimulatorPreferences {
                requires_approval: self.simulators.requires_approval,
                approval_lifetime: self.simulators.approval_lifetime,
                ..Default::default()
            },
            auto_cleanup: self.auto_cleanup,
            pr_cleanup: self.pr_cleanup,
            ..Default::default()
        }
    }

    fn parse(text: &str, paths: &Paths) -> Result<Self> {
        let config: Self = toml::from_str(text)?;
        crate::ai::validate(&config.ai, &paths.home)?;
        config.root_dir(paths)?;
        if let Some(minutes) = config.auto_cleanup.idle_minutes {
            validate_idle_minutes(minutes)?;
        }
        validate_port_range(config.ports.start, config.ports.end)?;
        config.simulators.validate()?;
        config.git.validate()?;
        config.agent_auth.validate()?;
        for &kind in crate::hooks::HookKind::ALL {
            if kind.allows_global() {
                kind.validate(kind.global_command(&config))?;
            }
        }
        named_commands::validate(&config.commands)?;
        if let Some(name) = &config.git_profile {
            config.git.profile(name)?;
        }
        crate::daemon::resources::definitions(&config.resources, &config.resource_pools)?;
        Ok(config)
    }

    /// The settings in effect for a target whose repository layers are
    /// `layers`: those over this machine's, then the built-in defaults.
    pub fn resolve(&self, layers: &ConfigLayers) -> Result<Effective> {
        resolve::Stack::new(self, layers).resolve()
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

#[derive(Debug, Serialize)]
pub struct PrCleanup {
    pub enabled: bool,
}
impl Default for PrCleanup {
    fn default() -> Self {
        Self { enabled: true }
    }
}
