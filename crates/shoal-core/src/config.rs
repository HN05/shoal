//! Shoal's configuration: the global file, the repository schema its
//! repository layers share, and the settings they resolve to.
pub mod agents;
pub mod apps;
pub mod edit;
pub mod forges;
pub mod gate;
pub mod git_profiles;
pub mod hooks;
pub mod machine;
pub mod mcp;
pub mod memory_limit;
pub mod modes;
pub mod named_commands;
pub mod overload;
mod parse;
pub mod permissions;
pub mod presets;
pub mod proxies;
pub mod repo;
pub mod resolve;
pub mod same_as;
pub mod shared;
pub mod span;
pub mod templates;
#[cfg(test)]
mod tests;
pub mod value;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use crate::paths::Paths;
pub use machine::{Machine, MachineLimit};
pub use repo::{
    AutoCleanup, Cleanup, Herdr, Issue, MergedCleanup, Ports, Restore, Rex, Runner, Settings,
    Simulators, Tmux, Zellij,
};
use repo::{ConfigLayers, RepoConfig};
pub use resolve::Effective;
use resolve::GlobalLayer;
pub use shared::Shared;
pub use shoal_protocol::config::options;

/// How many of a repository's heavy agents may run at once: a fixed
/// number, or under `dynamic` and `none` only the machine's limit.
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
/// for the options a repository may override, in the repository schema,
/// over the shared config's.
#[derive(Debug, Default)]
pub struct Config {
    /// This machine's own file's `[machine]` over the shared config's.
    pub machine: Machine,
    pub repository: RepoConfig,
    /// The file each mode's instructions or rule's text came from, once
    /// [`Self::load_with_templates`] read them.
    pub template_files: modes::Files,
    /// The shared config's values for the options a repository may
    /// override, under [`Self::repository`].
    pub shared: RepoConfig,
    /// The file each of the shared config's texts came from.
    pub shared_template_files: modes::Files,
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
        let path = crate::fsutil::home::expand_home(path, &paths.home);
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
        let own_home = crate::fsutil::home::home_dir().is_ok_and(|own| own == home);
        let config_home = std::env::var_os("XDG_CONFIG_HOME").filter(|_| own_home);
        config_home
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home.join(".config"))
            .join("shoal/config.toml")
    }

    /// The global file over the shared config as one snapshot: machine
    /// policy, without the prompt template files only agent launches need.
    pub fn load(paths: &Paths) -> Result<Self> {
        let shared = Shared::load(paths)?;
        let path = Self::path(paths);
        let text = read_file(&path)?;
        Self::parse_over(&text, &shared, paths).with_context(|| format!("parse {}", path.display()))
    }

    /// Both files as written, layered without validating their values, as
    /// skill delivery reads them without daemon paths.
    pub fn load_unchecked(home: &Path) -> Result<Self> {
        let path = Shared::path_for_home(home);
        let shared = parse::shared(&read_file(&path)?)
            .with_context(|| format!("parse {}", path.display()))?;
        let path = Self::path_for_home(home);
        let own = parse::global(&read_file(&path)?)
            .with_context(|| format!("parse {}", path.display()))?;
        Ok(Self {
            machine: Machine::layered(shared.machine, own.machine)?,
            repository: own.repository,
            shared: shared.repository,
            ..Default::default()
        })
    }

    /// [`Self::load`] with the template files beside each file standing in
    /// for its omitted inline templates, as the CLI reads it for a launch.
    pub fn load_with_templates(paths: &Paths) -> Result<Self> {
        let mut config = Self::load(paths)?;
        let [shared, own] = template_directories(paths)?;
        config.shared_template_files = modes::fill(&mut config.shared, &[shared])?;
        config.template_files = modes::fill(&mut config.repository, &[own])?;
        Ok(config)
    }

    /// The global files' values for the options a repository may override,
    /// the shared config's below this machine's, without their triggers,
    /// which are their own and never a repository's.
    pub fn global_layers(&self) -> [GlobalLayer; 2] {
        let layer = |config: &RepoConfig, files: &modes::Files| GlobalLayer {
            config: RepoConfig {
                triggers: Default::default(),
                ..config.clone()
            },
            files: files.clone(),
        };
        [
            layer(&self.shared, &self.shared_template_files),
            layer(&self.repository, &self.template_files),
        ]
    }

    /// The global files' triggers, each starting in the repository it
    /// names or in a chat without one; this machine's file replaces a
    /// shared trigger of the same name.
    pub fn triggers(&self) -> crate::triggers::Triggers {
        let mut triggers = self.shared.triggers.clone();
        triggers.extend(self.repository.triggers.clone());
        triggers
    }

    /// This machine's settings over the built-in defaults, without a
    /// repository's layers, with the global files' references followed.
    pub fn global_settings(&self) -> Result<Settings> {
        resolve::Stack::new(self, &ConfigLayers::default())?.resolve()
    }

    /// Every AI tool as this machine's config describes it, without a
    /// repository's layers.
    pub fn tools(&self) -> Result<crate::ai::Tools> {
        let settings = self.global_settings()?;
        Ok(crate::ai::tools(&agents::describe(
            &settings.agents,
            &self.machine.agents,
        )))
    }

    /// `text` as this machine's global file, without a shared config.
    pub fn parse(text: &str, paths: &Paths) -> Result<Self> {
        Self::parse_over(text, &Shared::default(), paths)
    }

    /// `text` as this machine's global file over `shared`.
    pub fn parse_over(text: &str, shared: &Shared, paths: &Paths) -> Result<Self> {
        Self::checked(shared, Some(parse::global(text)?), paths)
    }

    /// The config of `own`, this machine's global file, over `shared`, or
    /// of the shared config alone, as a machine whose own file sets nothing
    /// reads it, once its values are valid together.
    fn checked(shared: &Shared, own: Option<parse::GlobalFile>, paths: &Paths) -> Result<Self> {
        let [shared_directory, own_directory] = template_directories(paths)?;
        let directories = [
            Some(shared_directory),
            own.is_some().then_some(own_directory),
        ];
        let own = own.unwrap_or_default();
        let config = Self {
            machine: Machine::layered(shared.machine.clone(), own.machine)?,
            repository: own.repository,
            shared: shared.repository.clone(),
            ..Default::default()
        };
        config.validate(paths)?;
        // A global file is complete with the mode and rule files beside it:
        // its bounds must combine with the built-in defaults, so
        // `config set ports.end 4000` is refused rather than failing every
        // later command, and its modes may name rules only a file defines.
        // The config keeps what it says itself, as `config set` writes it.
        let layers = config.filled_layers(directories)?;
        let resolved = config.resolve_over(layers, &ConfigLayers::default())?;
        modes::check(&resolved.modes, &resolved.rules)?;
        Ok(config)
    }

    /// The file's two parts as written, without validating their values.
    pub fn from_toml(text: &str) -> Result<Self> {
        let parse::GlobalFile {
            machine,
            repository,
        } = parse::global(text)?;
        Ok(Self {
            machine: machine.read()?,
            repository,
            ..Default::default()
        })
    }

    fn validate(&self, paths: &Paths) -> Result<()> {
        let machine = &self.machine;
        let layers = [&self.shared, &self.repository];
        for layer in layers {
            layer.validate()?;
        }
        crate::ai::validate::skill_dirs(&machine.agents, &paths.home)?;
        crate::config::proxies::validate(&machine.proxies)?;
        crate::config::proxies::validate_references(&machine.agents, &machine.proxies)?;
        self.root_dir(paths)?;
        machine.overload.validate()?;
        machine.simulators.validate()?;
        machine.git_profiles.validate()?;
        machine.forges.validate()?;
        machine.app_store.validate()?;
        let profiles = layers.map(|layer| [&layer.git_profile, &layer.agent.git_profile]);
        for name in profiles.into_iter().flatten().flatten() {
            machine.git_profiles.profile(name)?;
        }
        Ok(())
    }

    /// The settings in effect for a target whose repository layers are
    /// `layers`: those over this machine's, then the built-in defaults.
    pub fn resolve(&self, layers: &ConfigLayers) -> Result<Effective> {
        self.resolve_over(self.global_layers(), layers)
    }

    /// [`Self::resolve`] with the template files beside each global file
    /// standing in for its omitted inline templates, as a loaded snapshot
    /// lacks them.
    pub fn resolve_with_template_files(
        &self,
        layers: &ConfigLayers,
        paths: &Paths,
    ) -> Result<Effective> {
        let directories = template_directories(paths)?.map(Some);
        let settings = self.resolve_over(self.filled_layers(directories)?, layers)?;
        modes::check(&settings.modes, &settings.rules)?;
        Ok(settings)
    }

    /// [`Self::global_layers`] with the mode and rule files in each layer's
    /// directory, where there is one, standing in for its omitted inline
    /// texts.
    fn filled_layers(&self, directories: [Option<PathBuf>; 2]) -> Result<[GlobalLayer; 2]> {
        let mut layers = self.global_layers();
        for (layer, directory) in layers.iter_mut().zip(directories) {
            if let Some(directory) = directory {
                layer.fill(&directory)?;
            }
        }
        Ok(layers)
    }

    /// The settings of `global`, the global files' layers, under `layers`,
    /// with this machine's tools.
    fn resolve_over(&self, global: [GlobalLayer; 2], layers: &ConfigLayers) -> Result<Effective> {
        let settings = resolve::Stack::over(global, layers)?.resolve()?;
        let agents = agents::describe(&settings.agents, &self.machine.agents);
        crate::ai::validate(&agents)?;
        let tools = crate::ai::tools(&agents);
        crate::config::proxies::validate_routes(&tools, &self.machine.proxies)?;
        let proxies = crate::config::proxies::routes(&tools, &self.machine.proxies);
        let agent_secrets = crate::secrets::for_agents(&settings, &self.triggers(), layers);
        Ok(Effective::new(settings, tools, proxies, agent_secrets))
    }
}

/// The global config read at launch and the settings it yields under the
/// repository `layers`. The global config is read now, so launch defaults
/// follow it without a daemon restart.
pub fn configuration(paths: &Paths, layers: &ConfigLayers) -> Result<(Config, Effective)> {
    let config = Config::load_with_templates(paths)?;
    let settings = config.resolve(layers)?;
    // With every layer's mode and rule files read, a launch refuses a mode
    // naming a rule none defines before it creates anything.
    modes::check(&settings.modes, &settings.rules)?;
    Ok((config, settings))
}

/// The directories whose mode and rule files fill in the global files'
/// omitted inline texts: the shared config's, then this machine's.
fn template_directories(paths: &Paths) -> Result<[PathBuf; 2]> {
    let path = Config::path(paths);
    let directory = path.parent().context("config has no directory")?;
    Ok([directory.join(shared::TEMPLATES_DIR), directory.to_owned()])
}

/// The text of the config file at `path`, empty while there is none.
fn read_file(path: &Path) -> Result<String> {
    let text =
        crate::fsutil::read_optional(path).with_context(|| format!("read {}", path.display()))?;
    Ok(text.unwrap_or_default())
}

/// Named templates shipped in this binary, independent of the source checkout.
pub const PACKAGED: &[(&str, &str)] = include!(concat!(env!("OUT_DIR"), "/packaged_configs.rs"));
