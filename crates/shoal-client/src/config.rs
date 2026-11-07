pub mod agents;
pub mod edit;
pub mod forges;
pub mod git_profiles;
pub mod hooks;
pub mod machine;
pub mod modes;
pub mod named_commands;
pub mod options;
pub mod overload;
mod parse;
pub mod placeholders;
pub mod presets;
pub mod proxies;
pub mod repo;
pub mod resolve;
pub mod templates;
#[cfg(test)]
mod tests;
pub mod value;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use crate::paths::Paths;
pub use machine::Machine;
pub use repo::{
    AutoCleanup, Cleanup, Herdr, Issue, MergedCleanup, Ports, Restore, Rex, Runner, Settings,
    Simulators, Tmux, Zellij,
};
use repo::{ConfigLayers, RepoConfig};
pub use resolve::Effective;

/// How many tracked agents, or CI jobs, may run concurrently.
///
/// Numeric values retain the original fixed limit. `dynamic` starts them
/// while measured load allows, while `none` disables admission limits for
/// the selected layer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RunLimit {
    #[default]
    Dynamic,
    Fixed(std::num::NonZeroU32),
    Unlimited,
}

impl Serialize for RunLimit {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Dynamic => serializer.serialize_str("dynamic"),
            Self::Fixed(value) => serializer.serialize_u32(value.get()),
            Self::Unlimited => serializer.serialize_str("none"),
        }
    }
}

impl<'de> Deserialize<'de> for RunLimit {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Count(u32),
            Name(String),
        }

        match Raw::deserialize(deserializer)? {
            Raw::Count(value) => std::num::NonZeroU32::new(value)
                .map(Self::Fixed)
                .ok_or_else(|| serde::de::Error::custom("the limit must be positive")),
            Raw::Name(name) => match name.as_str() {
                "dynamic" => Ok(Self::Dynamic),
                "none" | "unlimited" => Ok(Self::Unlimited),
                _ => Err(serde::de::Error::custom(
                    "the limit must be a positive number, 'dynamic', or 'none'",
                )),
            },
        }
    }
}

/// The global config: settings that describe this machine, and its values
/// for the options a repository may override, in the repository schema.
#[derive(Debug, Default)]
pub struct Config {
    pub machine: Machine,
    pub repository: RepoConfig,
    /// The file each mode's instructions or rule's text came from, once
    /// [`Self::load_with_templates`] read them.
    pub template_files: modes::Files,
}

impl Ports {
    pub fn validate(&self) -> Result<()> {
        validate_port_range(Some(self.start), Some(self.end))
    }
}

/// Agent directories are absolute or under the home directory (`~/`), since
/// agents run in different working directories.
pub fn validate_agent_dirs(directories: &[String]) -> Result<()> {
    for directory in directories {
        ensure!(
            Path::new(directory).is_absolute() || directory.starts_with("~/"),
            "agent.dirs entry {directory} must be an absolute path or start with ~/"
        );
    }
    Ok(())
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

impl Cleanup {
    /// How long a workspace stays eligible before automatic cleanup removes it.
    pub fn grace(&self) -> Duration {
        Duration::from_secs(self.grace_minutes * 60)
    }
}

impl AutoCleanup {
    /// The idle delay before removal; `None` when disabled.
    pub fn delay(&self) -> Option<Duration> {
        self.enabled
            .then(|| Duration::from_secs(self.idle_minutes * 60))
    }
}

pub fn validate_idle_minutes(key: &str, minutes: u64) -> Result<()> {
    ensure!(
        minutes > 0 && minutes <= 525600,
        "{key} must be between 1 and 525600"
    );
    Ok(())
}

impl Restore {
    pub fn retention(&self) -> Duration {
        Duration::from_secs(self.retention_days * 24 * 3600)
    }

    pub fn archive_max_bytes(&self) -> u64 {
        self.archive_max_mb * 1024 * 1024
    }
}

impl MergedCleanup {
    /// The idle period before removal; `None` when disabled.
    pub fn delay(&self) -> Option<Duration> {
        self.enabled
            .then(|| Duration::from_secs(self.idle_minutes * 60))
    }
}

impl Config {
    /// Parent of every repository directory: `<root>/<repo>/` holds the
    /// repository's URL clone as `.checkout` and its workspaces as siblings.
    pub fn root_dir(&self, paths: &Paths) -> Result<PathBuf> {
        let Some(path) = &self.machine.root_dir else {
            return Ok(paths.home.join("shoal"));
        };
        let path = crate::fsutil::expand_home(path, &paths.home);
        ensure!(
            path.is_absolute(),
            "machine.root_dir must be an absolute path or start with ~/"
        );
        Ok(path)
    }

    /// `$XDG_CONFIG_HOME/shoal/config.toml`, or `~/.config/shoal/config.toml`.
    pub fn path(paths: &Paths) -> PathBuf {
        Self::path_for_home(&paths.home)
    }

    /// `XDG_CONFIG_HOME` belongs to the home `HOME` names: another home, such
    /// as a test's temporary one, keeps its config under its own `.config`.
    pub fn path_for_home(home: &std::path::Path) -> PathBuf {
        let own_home = crate::paths::home_dir().is_ok_and(|own| own == home);
        let config_home = std::env::var_os("XDG_CONFIG_HOME").filter(|_| own_home);
        config_home
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

    /// The global file as one snapshot: machine policy, without the prompt
    /// template files only agent launches need.
    pub fn load(paths: &Paths) -> Result<Self> {
        let path = Self::path(paths);
        let Some(text) = crate::fsutil::read_optional(&path)
            .with_context(|| format!("read {}", path.display()))?
        else {
            return Self::parse("", paths);
        };
        Self::parse(&text, paths).with_context(|| format!("parse {}", path.display()))
    }

    /// [`Self::load`] with the template files beside the config standing in
    /// for omitted inline templates, as the CLI reads it for a launch.
    pub fn load_with_templates(paths: &Paths) -> Result<Self> {
        let mut config = Self::load(paths)?;
        config.template_files = read_template_files(&mut config.repository, paths)?;
        Ok(config)
    }

    /// This machine's repository-overridable settings as the layer below the
    /// repository's own.
    pub fn repository_layer(&self) -> RepoConfig {
        self.repository.clone()
    }

    /// Every AI tool as this machine's config describes it, without a
    /// repository's layers.
    pub fn tools(&self) -> Result<crate::ai::Tools> {
        let settings = resolve::Stack::new(self, &ConfigLayers::default()).resolve()?;
        Ok(crate::ai::tools(&agents::describe(
            &settings.agents,
            &self.machine.agents,
        )))
    }

    pub fn parse(text: &str, paths: &Paths) -> Result<Self> {
        let mut config = Self::from_toml(text)?;
        let machine = &mut config.machine;
        let written: toml::Table = toml::from_str(text)?;
        let overload = written
            .get("machine")
            .and_then(toml::Value::as_table)
            .and_then(|machine| machine.get("overload"))
            .and_then(toml::Value::as_table);
        machine.overload.follow_load(
            machine.load,
            overload::swap(),
            overload.unwrap_or(&toml::Table::new()),
        );
        config.validate(paths)?;
        // A global file is complete with the mode and rule files beside it:
        // its bounds must combine with the built-in defaults, so
        // `config set ports.end 4000` is refused rather than failing every
        // later command, and its modes may name rules only a file defines.
        // The config keeps what it says itself, as `config set` writes it.
        let written = config.repository.clone();
        read_template_files(&mut config.repository, paths)?;
        let resolved = config.resolve(&ConfigLayers::default());
        config.repository = written;
        let resolved = resolved?;
        modes::check(&resolved.modes, &resolved.rules)?;
        Ok(config)
    }

    /// The file's two parts as written, without validating their values.
    pub fn from_toml(text: &str) -> Result<Self> {
        let (machine, repository) = parse::global(text)?;
        Ok(Self {
            machine,
            repository,
            template_files: modes::Files::new(),
        })
    }

    fn validate(&self, paths: &Paths) -> Result<()> {
        let (machine, repository) = (&self.machine, &self.repository);
        repository.validate()?;
        crate::ai::validate::skill_dirs(&machine.agents, &paths.home)?;
        crate::config::proxies::validate(&machine.proxies)?;
        crate::config::proxies::validate_references(&machine.agents, &machine.proxies)?;
        self.root_dir(paths)?;
        machine.overload.validate()?;
        machine.simulators.validate()?;
        machine.git_profiles.validate()?;
        machine.forges.validate()?;
        for name in [&repository.git_profile, &repository.agent.git_profile]
            .into_iter()
            .flatten()
        {
            machine.git_profiles.profile(name)?;
        }
        Ok(())
    }

    /// The settings in effect for a target whose repository layers are
    /// `layers`: those over this machine's, then the built-in defaults.
    pub fn resolve(&self, layers: &ConfigLayers) -> Result<Effective> {
        self.resolve_over(self.repository_layer(), self.template_files.clone(), layers)
    }

    /// [`Self::resolve`] with the template files beside the config standing
    /// in for omitted inline templates, as a loaded snapshot lacks them.
    pub fn resolve_with_template_files(
        &self,
        layers: &ConfigLayers,
        paths: &Paths,
    ) -> Result<Effective> {
        let mut global = self.repository_layer();
        let files = read_template_files(&mut global, paths)?;
        let settings = self.resolve_over(global, files, layers)?;
        modes::check(&settings.modes, &settings.rules)?;
        Ok(settings)
    }

    /// The settings of `global`, this config's repository layer with its
    /// templates read from `files`, under `layers`, with this machine's tools.
    fn resolve_over(
        &self,
        global: RepoConfig,
        files: modes::Files,
        layers: &ConfigLayers,
    ) -> Result<Effective> {
        let settings = resolve::Stack::over(global, files, layers).resolve()?;
        let agents = agents::describe(&settings.agents, &self.machine.agents);
        crate::ai::validate(&agents, &settings.commands)?;
        let tools = crate::ai::tools(&agents);
        let proxies = crate::config::proxies::routes(&tools, &self.machine.proxies);
        Ok(Effective::new(settings, tools, proxies))
    }
}

/// Fill `global`'s omitted inline instructions and rule texts from the
/// files beside the global config, returning the file each came from.
fn read_template_files(global: &mut RepoConfig, paths: &Paths) -> Result<modes::Files> {
    let path = Config::path(paths);
    let directory = path.parent().context("config has no directory")?;
    modes::fill(global, &[directory.to_owned()])
}

fn default_template() -> &'static str {
    PACKAGED
        .iter()
        .find(|(name, _)| *name == "default")
        .expect("packaged default config")
        .1
}

/// Named templates shipped in this binary, independent of the source checkout.
pub const PACKAGED: &[(&str, &str)] = include!(concat!(env!("OUT_DIR"), "/packaged_configs.rs"));
