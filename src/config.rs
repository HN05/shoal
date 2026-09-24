pub mod edit;
pub mod named_commands;
mod placeholders;
pub mod repo;
pub mod report;
pub mod resolve;
pub mod templates;
#[cfg(test)]
mod tests;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf, time::Duration};

use crate::fsutil::{self, Permissions, ReplaceOptions};
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
    pub default_agent: Option<crate::agent::Agent>,
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
    pub default_mode: crate::agent::CodexMode,
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

/// The repository-overridable simulator settings after every layer.
#[derive(Debug, Default, Clone, Serialize)]
pub struct Simulators {
    pub requires_approval: bool,
    pub approval_lifetime: crate::daemon::access::Lifetime,
    pub preferred: Vec<String>,
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

#[derive(Debug, Serialize)]
pub struct PrCleanup {
    pub enabled: bool,
}
impl Default for PrCleanup {
    fn default() -> Self {
        Self { enabled: true }
    }
}

impl Config {
    /// Parent of every repository directory: `<root>/<repo>/` holds the
    /// repository's URL clone as `.checkout` and its workspaces as siblings.
    pub fn root_dir(&self, paths: &Paths) -> Result<PathBuf> {
        let Some(path) = self.root_dir.as_ref().or(self.repositories_dir.as_ref()) else {
            return Ok(paths.home.join("shoal"));
        };
        let path = crate::fsutil::expand_home(path, &paths.home);
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
        let text = crate::fsutil::read_optional(&path)
            .with_context(|| format!("read {}", path.display()))?
            .unwrap_or_default();
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
        let temporary = fsutil::prepare_atomic_write(
            &path,
            text.as_bytes(),
            ReplaceOptions {
                permissions: Permissions::Temporary,
                sync: false,
            },
        )?;
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

    /// The global file as one snapshot: machine policy, without the prompt
    /// template files only agent launches need.
    pub fn load(paths: &Paths) -> Result<Self> {
        let path = Self::path(paths);
        let Some(text) = crate::fsutil::read_optional(&path)
            .with_context(|| format!("read {}", path.display()))?
        else {
            return Ok(Self::default());
        };
        Self::parse(&text, paths).with_context(|| format!("parse {}", path.display()))
    }

    /// [`Self::load`] with the template files beside the config standing in
    /// for omitted inline templates, as the CLI reads it for a launch.
    pub fn load_with_templates(paths: &Paths) -> Result<Self> {
        let mut config = Self::load(paths)?;
        let directory = Self::path(paths);
        let directory = directory.parent().context("config has no directory")?;
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
        // A global file is complete on its own: its bounds must combine with
        // the built-in defaults, so `config set ports.end 4000` is refused
        // rather than failing every later command.
        config.resolve(&ConfigLayers::default())?;
        Ok(config)
    }

    /// The settings in effect for a target whose repository layers are
    /// `layers`: those over this machine's, then the built-in defaults.
    pub fn resolve(&self, layers: &ConfigLayers) -> Result<Effective> {
        resolve::Stack::new(self, layers).resolve()
    }
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
