pub mod edit;
pub mod forges;
pub mod named_commands;
pub mod overload;
pub(crate) mod placeholders;
pub mod repo;
pub mod report;
pub mod resolve;
pub mod templates;
#[cfg(test)]
mod tests;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, path::PathBuf, time::Duration};

use crate::fsutil::{self, Permissions, ReplaceOptions};
use crate::paths::Paths;
use repo::{ConfigLayers, RepoConfig};
pub use resolve::Effective;

/// How many tracked agents may run concurrently.
///
/// Numeric values retain the original fixed limit. `dynamic` starts agents
/// while measured load allows, while `none` disables admission limits for
/// the selected layer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AgentLimit {
    #[default]
    Dynamic,
    Fixed(std::num::NonZeroU32),
    Unlimited,
}

impl Serialize for AgentLimit {
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

impl<'de> Deserialize<'de> for AgentLimit {
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
                .ok_or_else(|| serde::de::Error::custom("max_agents must be positive")),
            Raw::Name(name) => match name.as_str() {
                "dynamic" => Ok(Self::Dynamic),
                "none" | "unlimited" => Ok(Self::Unlimited),
                _ => Err(serde::de::Error::custom(
                    "max_agents must be a positive number, 'dynamic', or 'none'",
                )),
            },
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub ai: crate::ai::Agents,
    pub commands: named_commands::Commands,
    pub agent_resume: named_commands::Commands,
    /// Retired with issue templates; read so configs written by earlier
    /// versions still load and `config show` can say it is unused.
    #[serde(rename = "issue_template")]
    pub retired_issue_template: Option<String>,
    pub worker_template: Option<String>,
    /// What `worker_template` was called; still read, with a rename warning.
    #[serde(rename = "agent_template")]
    pub renamed_agent_template: Option<String>,
    pub chat_template: Option<String>,
    pub agent_auth: crate::agent_auth::Config,
    pub bitbucket: crate::forge::backends::bitbucket::Credentials,
    /// Forge hosts whose type is set instead of asked.
    pub forges: forges::Forges,
    pub git: crate::git_profile::Git,
    pub git_profile: Option<String>,
    pub copy_ignored: Option<bool>,
    pub pre_setup_cmd: Option<String>,
    pub post_setup_cmd: Option<String>,
    pub pre_remove_cmd: Option<String>,
    pub post_remove_cmd: Option<String>,
    pub post_done_cmd: Option<String>,
    pub post_ready_cmd: Option<String>,
    pub post_agent_exit_cmd: Option<String>,
    pub post_resource_acquire_cmd: Option<String>,
    pub pre_resource_release_cmd: Option<String>,
    pub root_dir: Option<PathBuf>,
    /// Former name of `root_dir`; still accepted so existing configs load.
    pub repositories_dir: Option<PathBuf>,
    /// Agent an issue workspace starts when `--agent` is omitted.
    pub default_agent: Option<crate::agent::Agent>,
    /// Agents running at once on this machine; defaults to measured load and
    /// `shoal add` queues past it. Use `none` for unlimited admission.
    pub max_agents: Option<AgentLimit>,
    /// How hard Shoal may load this machine; sets `[overload]` defaults.
    pub load: overload::Load,
    /// Hold off sleep, including from a closed laptop lid, while agents have work.
    pub keep_awake: bool,
    /// Whether new tabs and sessions take the terminal; unset follows the launch.
    pub focus: Option<bool>,
    pub codex: repo::Codex,
    pub herdr: repo::Herdr,
    pub happy: repo::Happy,
    pub overload: overload::Overload,
    pub cleanup: repo::Cleanup,
    /// Former names of `[cleanup.auto]` and `[cleanup.pr]`; parsing folds them
    /// into `cleanup` so existing configs load.
    pub auto_cleanup: repo::AutoCleanup,
    pub pr_cleanup: repo::PrCleanup,
    pub done: repo::Done,
    pub review: repo::Review,
    pub ready: repo::Ready,
    pub unready: repo::Unready,
    pub pr: repo::Pr,
    pub issue: repo::Issue,
    pub land: repo::Land,
    pub approvals: repo::Approvals,
    pub api: repo::Api,
    pub ports: PortRange,
    /// Environment variables layered per repository and rendered for a workspace.
    pub env: BTreeMap<String, String>,
    pub caches: BTreeMap<String, repo::Cache>,
    /// Build tools whose usual setup every repository's workspaces share
    /// unless the repository names its own.
    pub presets: Option<Vec<crate::presets::Preset>>,
    pub secrets: crate::secrets::Secrets,
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
    pub approval_lifetime: crate::daemon::approvals::Lifetime,
    pub preferred: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct Cleanup {
    pub grace_minutes: u64,
    pub auto: AutoCleanup,
    pub pr: PrCleanup,
    pub merged: MergedCleanup,
    pub restore: Restore,
}

impl Default for Cleanup {
    fn default() -> Self {
        Self {
            grace_minutes: 10,
            auto: AutoCleanup::default(),
            pr: PrCleanup::default(),
            merged: MergedCleanup::default(),
            restore: Restore::default(),
        }
    }
}

impl Cleanup {
    /// How long a workspace stays eligible before automatic cleanup removes it.
    pub fn grace(&self) -> Duration {
        Duration::from_secs(self.grace_minutes * 60)
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

pub fn validate_idle_minutes(key: &str, minutes: u64) -> Result<()> {
    ensure!(
        minutes > 0 && minutes <= 525600,
        "{key} must be between 1 and 525600"
    );
    Ok(())
}

/// Removal of merged workspaces whether or not their assignment is done.
#[derive(Debug, Serialize)]
pub struct MergedCleanup {
    pub enabled: bool,
    pub idle_minutes: u64,
}

impl Default for MergedCleanup {
    fn default() -> Self {
        Self {
            enabled: true,
            idle_minutes: 240,
        }
    }
}

/// How long checkpoints and removed workspaces stay restorable, and how
/// much of the unsaved files a checkpoint keeps.
#[derive(Debug, Serialize)]
pub struct Restore {
    pub retention_days: u64,
    pub archive_max_mb: u64,
}

impl Default for Restore {
    fn default() -> Self {
        Self {
            retention_days: 30,
            archive_max_mb: 50,
        }
    }
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

#[derive(Debug, Serialize)]
pub struct PrCleanup {
    pub enabled: bool,
}
impl Default for PrCleanup {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Serialize)]
pub struct Review {
    pub post: bool,
}

impl Default for Review {
    fn default() -> Self {
        Self { post: true }
    }
}

#[derive(Debug, Serialize)]
pub struct Ready {
    pub undraft: bool,
}

impl Default for Ready {
    fn default() -> Self {
        Self { undraft: true }
    }
}

#[derive(Debug, Default, Serialize)]
pub struct Unready {
    pub draft: bool,
}

#[derive(Debug, Default, Serialize)]
pub struct Pr {
    pub draft: bool,
    /// Entries a new PR's labels must each start with.
    pub required_labels: Vec<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct Issue {
    /// Entries a new issue's labels must each start with.
    pub required_labels: Vec<String>,
    /// Whether scoped callers may open issues in other repositories.
    pub other_repositories: repo::OtherRepositories,
    /// Marks an issue that needs a plan before anyone implements it.
    pub plan_label: Option<String>,
    /// Marks an issue whose body holds a settled plan.
    pub planned_label: Option<String>,
}

/// Whether `shoal forge api` sends an agent's requests that are not reads.
#[derive(Debug, Default, Serialize)]
pub struct Api {
    pub agent_writes: bool,
}

/// Which agent forge writes wait for the user's approval.
#[derive(Debug, Default, Serialize)]
pub struct Approvals {
    pub issue_open: repo::Opening,
    pub pr_open: repo::Opening,
}

#[derive(Debug, Serialize)]
pub struct Land {
    pub push: bool,
    pub requires_approval: bool,
}

impl Default for Land {
    fn default() -> Self {
        Self {
            push: false,
            requires_approval: true,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Done {
    pub cleanup: bool,
}

impl Default for Done {
    fn default() -> Self {
        Self { cleanup: true }
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
        // Unit tests inherit the developer's environment; keep them in their
        // temporary home instead of the real config.
        let config_home = if cfg!(test) {
            None
        } else {
            std::env::var_os("XDG_CONFIG_HOME")
        };
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

    /// Write a fresh template, first moving any existing file to
    /// `config.toml.backup` (replacing an older backup). Returns the path and
    /// the backup, if one was made.
    pub fn reset(paths: &Paths) -> Result<(PathBuf, Option<PathBuf>)> {
        Self::install_named(paths, "default")
    }

    /// Apply `changes` together, saving only when the result is valid.
    pub fn edit(paths: &Paths, changes: &[edit::Change]) -> Result<(PathBuf, Option<PathBuf>)> {
        let path = Self::path(paths);
        let _lock = Self::lock_file(&path)?;
        let text = crate::fsutil::read_optional(&path)
            .with_context(|| format!("read {}", path.display()))?
            .unwrap_or_default();
        let edited = edit::apply(&text, changes)?;
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
            return Self::parse("", paths);
        };
        Self::parse(&text, paths).with_context(|| format!("parse {}", path.display()))
    }

    /// [`Self::load`] under the edit lock, so a concurrent edit is read whole
    /// rather than while its file is moved aside.
    pub fn load_settled(paths: &Paths) -> Result<Self> {
        let _lock = Self::lock_file(&Self::path(paths))?;
        Self::load(paths)
    }

    /// [`Self::load`] with the template files beside the config standing in
    /// for omitted inline templates, as the CLI reads it for a launch.
    pub fn load_with_templates(paths: &Paths) -> Result<Self> {
        let mut config = Self::load(paths)?;
        let directory = Self::path(paths);
        let directory = directory.parent().context("config has no directory")?;
        if config.retired_issue_template.is_none() {
            config.retired_issue_template =
                templates::read(directory, templates::RETIRED_ISSUE_FILE)?;
        }
        templates::read_worker(
            directory,
            &mut config.worker_template,
            &mut config.renamed_agent_template,
        )?;
        if config.chat_template.is_none() {
            config.chat_template = templates::read(directory, templates::CHAT_FILE)?;
        }
        Ok(config)
    }

    /// This machine's repository-overridable settings as the layer below the
    /// repository's own; machine-only settings stay out of it.
    pub fn repository_layer(&self) -> RepoConfig {
        RepoConfig {
            commands: self
                .commands
                .clone()
                .into_iter()
                .chain(crate::ai::commands(&self.ai))
                .collect(),
            agent_resume: self.agent_resume.clone(),
            retired_issue_template: self.retired_issue_template.clone(),
            worker_template: self.worker_template.clone(),
            renamed_agent_template: self.renamed_agent_template.clone(),
            chat_template: self.chat_template.clone(),
            agent_auth: self.agent_auth.clone(),
            git_profile: self.git_profile.clone(),
            default_agent: self.default_agent.clone(),
            max_agents: self.max_agents,
            focus: self.focus,
            codex: self.codex,
            herdr: self.herdr.clone(),
            happy: self.happy.clone(),
            copy_ignored: self.copy_ignored,
            pre_setup_cmd: self.pre_setup_cmd.clone(),
            post_setup_cmd: self.post_setup_cmd.clone(),
            pre_remove_cmd: self.pre_remove_cmd.clone(),
            post_remove_cmd: self.post_remove_cmd.clone(),
            post_done_cmd: self.post_done_cmd.clone(),
            post_ready_cmd: self.post_ready_cmd.clone(),
            post_agent_exit_cmd: self.post_agent_exit_cmd.clone(),
            post_resource_acquire_cmd: self.post_resource_acquire_cmd.clone(),
            pre_resource_release_cmd: self.pre_resource_release_cmd.clone(),
            ports: repo::PortDefaults {
                start: self.ports.start,
                end: self.ports.end,
                ..Default::default()
            },
            env: self.env.clone(),
            caches: self.caches.clone(),
            presets: self.presets.clone(),
            secrets: self.secrets.clone(),
            resources: self.resources.clone(),
            resource_pools: self.resource_pools.clone(),
            simulators: repo::SimulatorPreferences {
                requires_approval: self.simulators.requires_approval,
                approval_lifetime: self.simulators.approval_lifetime,
                ..Default::default()
            },
            cleanup: self.cleanup,
            done: self.done,
            review: self.review,
            ready: self.ready,
            unready: self.unready,
            pr: self.pr.clone(),
            issue: self.issue.clone(),
            land: self.land,
            approvals: self.approvals,
            api: self.api,
            ..Default::default()
        }
    }

    fn parse(text: &str, paths: &Paths) -> Result<Self> {
        let mut config: Self = toml::from_str(text)?;
        let written: toml::Table = toml::from_str(text)?;
        let overload = written.get("overload").and_then(toml::Value::as_table);
        config.overload.follow_load(
            config.load,
            crate::process::load::swap(),
            overload.unwrap_or(&toml::Table::new()),
        );
        config
            .cleanup
            .fold_legacy(&mut config.auto_cleanup, &mut config.pr_cleanup);
        crate::ai::validate(&config.ai, &config.commands, &paths.home)?;
        config.root_dir(paths)?;
        templates::ensure_one_worker(&config.worker_template, &config.renamed_agent_template)?;
        config.cleanup.validate()?;
        config.overload.validate()?;
        config.simulators.validate()?;
        config.git.validate()?;
        config.agent_auth.validate()?;
        config.bitbucket.validate()?;
        config.forges.validate()?;
        for &kind in crate::hooks::HookKind::ALL {
            if kind.allows_global() {
                kind.validate(kind.global_command(&config))?;
            }
        }
        named_commands::validate(&config.commands)?;
        named_commands::validate(&config.agent_resume)?;
        for name in [&config.git_profile, &config.agent_auth.git_profile]
            .into_iter()
            .flatten()
        {
            config.git.profile(name)?;
        }
        crate::daemon::resources::definitions(&config.resources, &config.resource_pools)?;
        crate::env::validate_configured_environment(&config.env)?;
        crate::caches::validate(&config.caches)?;
        crate::presets::validate(config.presets.as_deref().unwrap_or_default())?;
        crate::secrets::validate(&config.secrets)?;
        // A global file is complete on its own: its bounds must combine with
        // the built-in defaults, so `config set ports.end 4000` is refused
        // rather than failing every later command.
        config.resolve(&ConfigLayers::default())?;
        Ok(config)
    }

    /// The settings in effect for a target whose repository layers are
    /// `layers`: those over this machine's, then the built-in defaults.
    pub fn resolve(&self, layers: &ConfigLayers) -> Result<Effective> {
        let mut effective = resolve::Stack::new(self, layers).resolve()?;
        effective.providers = crate::ai::providers(&self.ai);
        effective.arguments = crate::ai::all_arguments(&self.ai);
        Ok(effective)
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

#[derive(Debug, Serialize)]
pub struct Happy {
    pub enabled: bool,
}

#[derive(Debug, Serialize)]
pub struct Herdr {
    pub enabled: bool,
    pub tab_name: Option<String>,
    pub new_tab: bool,
    pub close_when_done: bool,
}
