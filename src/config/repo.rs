use crate::agent::{Agent, AgentMode};
use crate::config::options::Options;
use crate::sessions::HostName;
use crate::state::states;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, path::Path};

#[derive(Debug, Serialize, Deserialize)]
pub struct LocalConfig {
    pub repository_id: String,
    pub toml: Option<String>,
}

/// The repository-owned configuration layers before they are resolved. Keeping
/// them separate lets callers explain where an effective value came from.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct ConfigLayers {
    pub worktree_file: RepoConfig,
    pub saved_repository_config: RepoConfig,
}

/// Display uses human-readable layer labels rather than the snake_case JSON names.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConfigLayer {
    BuiltInDefault,
    GlobalConfig,
    WorktreeFile,
    SavedRepositoryConfig,
}

impl ConfigLayer {
    /// Whether the repository sets this layer, rather than the machine.
    pub fn is_repository(self) -> bool {
        matches!(self, Self::WorktreeFile | Self::SavedRepositoryConfig)
    }
}

impl std::fmt::Display for ConfigLayer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::BuiltInDefault => "built-in default",
            Self::GlobalConfig => "global config",
            Self::WorktreeFile => "worktree file",
            Self::SavedRepositoryConfig => "saved repository config",
        })
    }
}

states!(
    #[derive(Default)]
    ConflictPolicy: ValueEnum {
        Auto => "auto",
        #[default]
        Suggest => "suggest",
    }
);

#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Settings)]
pub struct RepoConfig {
    #[option(named, default = crate::config::named_commands::defaults())]
    pub commands: crate::config::named_commands::Commands,
    #[option(named)]
    pub agent_resume: crate::config::named_commands::Commands,
    #[option(template)]
    pub worker_template: Option<String>,
    #[option(template, default = crate::config::templates::CHAT_DEFAULT.into())]
    pub chat_template: Option<String>,
    #[option(section)]
    pub agent_auth: crate::agent_auth::Config,
    pub git_profile: Option<String>,
    /// Agent an issue workspace starts when `--agent` is omitted.
    pub default_agent: Option<Agent>,
    /// Agents of this repository running at once; defaults to the host
    /// capacity and `shoal add` queues past it. Use `none` for unlimited.
    #[option(default = crate::config::AgentLimit::Dynamic)]
    pub max_agents: Option<crate::config::AgentLimit>,
    /// Whether new tabs and sessions take the terminal; unset follows the launch.
    pub focus: Option<bool>,
    /// Whether a place Shoal opened for a workspace closes once it is done.
    #[option(default = true)]
    pub close_when_done: Option<bool>,
    /// The terminal host for launches from a terminal no host serves.
    #[option(default = DefaultHost::default(), resolve = DefaultHost::resolve -> HostName)]
    pub default_host: Option<DefaultHost>,
    #[option(section)]
    pub claude: ShortcutConfig,
    #[option(section)]
    pub codex: ShortcutConfig,
    #[option(section)]
    pub herdr: HerdrConfig,
    #[option(section)]
    pub rex: RexConfig,
    #[option(section)]
    pub happy: HappyConfig,
    /// Copy the main checkout's ignored files into the worktree before setup.
    #[option(default = true)]
    pub copy_ignored: Option<bool>,
    /// Directories every agent and chat gets as other working directories.
    #[option(default = vec!["/tmp".into()])]
    pub agent_dirs: Option<Vec<String>>,
    pub setup_cmd: Option<String>,
    pub pre_setup_cmd: Option<String>,
    pub post_remove_cmd: Option<String>,
    pub post_done_cmd: Option<String>,
    pub post_ready_cmd: Option<String>,
    pub post_agent_exit_cmd: Option<String>,
    pub post_resource_acquire_cmd: Option<String>,
    pub pre_resource_release_cmd: Option<String>,
    /// Runs untracked after the workspace is ready, e.g. to open a tmux session.
    pub post_setup_cmd: Option<String>,
    /// Runs untracked before the worktree is removed, e.g. to close that session.
    pub pre_remove_cmd: Option<String>,
    #[option(section)]
    pub ports: PortsConfig,
    /// Environment variables rendered for a workspace, with the presets'.
    #[option(named, resolve = preset_environment)]
    pub env: BTreeMap<String, String>,
    #[option(named, resolve = preset_caches)]
    pub caches: BTreeMap<String, Cache>,
    /// Commands kept running for the user to try, by name.
    #[option(named)]
    pub demos: BTreeMap<String, crate::demos::Demo>,
    /// Build tools whose usual setup the repository's workspaces share; an
    /// empty list turns off presets a lower layer names. Unset when no layer
    /// names presets.
    pub presets: Option<Vec<crate::presets::Preset>>,
    /// Machine-specific, so the global config or saved repository config sets
    /// it, never the worktree file.
    #[option(named)]
    pub secrets: crate::secrets::Secrets,
    #[option(named)]
    pub resources: BTreeMap<String, crate::daemon::resources::ResourceConfig>,
    #[option(named)]
    pub resource_pools: BTreeMap<String, crate::daemon::resources::PoolConfig>,
    #[option(section)]
    pub simulators: SimulatorsConfig,
    #[option(section)]
    pub cleanup: CleanupConfig,
    #[option(section)]
    pub done: DoneConfig,
    #[option(section)]
    pub idle_stop: IdleStopConfig,
    #[option(section)]
    pub review: ReviewConfig,
    #[option(section)]
    pub ready: ReadyConfig,
    #[option(section)]
    pub unready: UnreadyConfig,
    #[option(section)]
    pub pr: PrConfig,
    #[option(section)]
    pub issue: IssueConfig,
    #[option(section)]
    pub land: LandConfig,
    #[option(section)]
    pub approvals: ApprovalsConfig,
    #[option(section)]
    pub api: ApiConfig,
    #[option(section)]
    pub seed: Seed,
}

fn preset_environment(
    env: BTreeMap<String, String>,
    config: &RepoConfig,
) -> Result<BTreeMap<String, String>> {
    let presets = config.presets.as_deref().unwrap_or_default();
    Ok(crate::presets::environment(presets, env))
}

fn preset_caches(
    caches: BTreeMap<String, Cache>,
    config: &RepoConfig,
) -> Result<BTreeMap<String, Cache>> {
    let presets = config.presets.as_deref().unwrap_or_default();
    Ok(crate::presets::caches(presets, caches))
}

states!(
    /// Who shares a cache directory.
    CacheScope {
        /// Every workspace of the repository.
        Shared => "shared",
    }
);

/// A tool cache directory `[env]` names with `{cache.NAME}`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Cache {
    pub scope: CacheScope,
}

/// The default-branch build new workspaces start from.
#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Self)]
pub struct Seed {
    /// Executable in the seed worktree that builds the default branch.
    pub cmd: Option<String>,
    /// Worktree-relative build outputs cloned into each new workspace.
    #[option(list)]
    pub paths: Vec<String>,
}

impl Seed {
    fn validate(&self) -> Result<()> {
        // The seed runs unattended from the default branch, so its command
        // is the repository's own file, like its outputs.
        if let Some(command) = &self.cmd {
            ensure!(
                inside_worktree(command),
                "seed.cmd must be a relative path inside the worktree: {command}"
            );
        }
        for (index, path) in self.paths.iter().enumerate() {
            ensure!(
                inside_worktree(path),
                "seed.paths entries must be relative paths inside the worktree: {path}"
            );
            // Each output is cloned whole, so one cannot contain another.
            for other in &self.paths[..index] {
                ensure!(
                    !Path::new(path).starts_with(other) && !Path::new(other).starts_with(path),
                    "seed.paths entries overlap: {other} and {path}"
                );
            }
        }
        Ok(())
    }
}

fn inside_worktree(path: &str) -> bool {
    !path.trim().is_empty()
        && !path.contains('\0')
        && Path::new(path).components().all(
            |component| matches!(component, std::path::Component::Normal(name) if name != ".git"),
        )
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default)]
#[options(resolved = Ports)]
pub struct PortsConfig {
    #[option(default = ConflictPolicy::default())]
    pub on_conflict: Option<ConflictPolicy>,
    /// Bounds of the automatic range.
    #[option(default = 49152)]
    pub start: Option<u16>,
    #[option(default = 65535)]
    pub end: Option<u16>,
    #[serde(flatten)]
    #[option(named)]
    pub definitions: BTreeMap<String, PortDefinition>,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PortDefinition {
    pub requires_approval: bool,
    pub approval_lifetime: crate::daemon::approvals::Lifetime,
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
    let mut config = match found.first() {
        Some(path) => {
            let text =
                fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
            let config = parse(&text).with_context(|| format!("parse {}", path.display()))?;
            ensure!(
                config.secrets.is_empty(),
                "{}: [secrets] belongs in the global or saved repository config",
                path.display()
            );
            config
        }
        None => RepoConfig::default(),
    };
    if config.worker_template.is_none() {
        config.worker_template =
            crate::config::templates::read(workspace_dir, crate::config::templates::WORKER_FILE)?;
    }
    if config.chat_template.is_none() {
        config.chat_template =
            crate::config::templates::read(workspace_dir, crate::config::templates::CHAT_FILE)?;
    }
    Ok(config)
}

impl RepoConfig {
    /// Warn about a worker template that repeats the built-in guidance.
    pub fn normalize(&self) -> Vec<&'static str> {
        let mut warnings = Vec::new();
        if self
            .worker_template
            .as_deref()
            .is_some_and(crate::config::templates::repeats_guidance)
        {
            warnings.push(
                "worker_template repeats Shoal's built-in guidance, which every launch \
                 already receives; keep only your additions",
            );
        }
        warnings
    }
}

pub fn parse(text: &str) -> Result<RepoConfig> {
    let config: RepoConfig = toml::from_str(text)?;
    crate::config::named_commands::validate(&config.commands)?;
    crate::config::named_commands::validate(&config.agent_resume)?;
    config.agent_auth.validate()?;
    if let Some(name) = &config.git_profile {
        crate::validate::name("git profile", name)?;
    }
    for &kind in crate::hooks::HookKind::ALL {
        kind.validate(kind.repository_command(&config))?;
    }
    config.cleanup.validate()?;
    crate::config::validate_port_range(config.ports.start, config.ports.end)?;
    for (name, definition) in &config.ports.definitions {
        crate::validate::lowercase_name("port", name)
            .with_context(|| format!("invalid configured port name: {name}"))?;
        ensure!(
            definition.port != Some(0),
            "configured port {name} cannot use port zero"
        );
    }
    crate::daemon::resources::definitions(&config.resources, &config.resource_pools)?;
    crate::env::validate_configured_environment(&config.env)?;
    crate::config::validate_agent_dirs(config.agent_dirs.as_deref().unwrap_or_default())?;
    crate::caches::validate(&config.caches)?;
    crate::demos::validate(&config.demos)?;
    crate::presets::validate(config.presets.as_deref().unwrap_or_default())?;
    crate::secrets::validate(&config.secrets)?;
    config.seed.validate()?;
    Ok(config)
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Simulators)]
pub struct SimulatorsConfig {
    #[option(default = false)]
    pub requires_approval: Option<bool>,
    #[option(default = crate::daemon::approvals::Lifetime::default())]
    pub approval_lifetime: Option<crate::daemon::approvals::Lifetime>,
    #[option(list)]
    pub preferred: Vec<String>,
}

/// What `shoal <agent>` does without `--cli` or `--app`, from the agent's
/// section.
#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Shortcut)]
pub struct ShortcutConfig {
    #[option(default = AgentMode::default())]
    pub default_mode: Option<AgentMode>,
}

#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Cleanup)]
pub struct CleanupConfig {
    #[option(default = 10)]
    pub grace_minutes: Option<u64>,
    #[option(section)]
    pub auto: AutoCleanupConfig,
    #[option(section)]
    pub pr: PrCleanupConfig,
    #[option(section)]
    pub merged: MergedCleanupConfig,
    #[option(section)]
    pub restore: RestoreConfig,
}

impl CleanupConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        if let Some(minutes) = self.grace_minutes {
            anyhow::ensure!(
                minutes <= 525600,
                "cleanup.grace_minutes must be at most 525600"
            );
        }
        for (key, minutes) in [
            ("cleanup.auto.idle_minutes", self.auto.idle_minutes),
            ("cleanup.merged.idle_minutes", self.merged.idle_minutes),
        ] {
            if let Some(minutes) = minutes {
                crate::config::validate_idle_minutes(key, minutes)?;
            }
        }
        if let Some(days) = self.restore.retention_days {
            anyhow::ensure!(
                (1..=3650).contains(&days),
                "cleanup.restore.retention_days must be between 1 and 3650"
            );
        }
        if let Some(megabytes) = self.restore.archive_max_mb {
            anyhow::ensure!(
                megabytes <= 10240,
                "cleanup.restore.archive_max_mb must be at most 10240"
            );
        }
        Ok(())
    }
}

#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = AutoCleanup)]
pub struct AutoCleanupConfig {
    #[option(default = true)]
    pub enabled: Option<bool>,
    #[option(default = 10)]
    pub idle_minutes: Option<u64>,
}

/// Removal of merged workspaces whether or not their assignment is done.
#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = MergedCleanup)]
pub struct MergedCleanupConfig {
    #[option(default = true)]
    pub enabled: Option<bool>,
    #[option(default = 240)]
    pub idle_minutes: Option<u64>,
}

/// How long checkpoints and removed workspaces stay restorable, and how
/// much of the unsaved files a checkpoint keeps.
#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Restore)]
pub struct RestoreConfig {
    #[option(default = 30)]
    pub retention_days: Option<u64>,
    #[option(default = 50)]
    pub archive_max_mb: Option<u64>,
}

/// Stopping any agent idle once its prompt cache expires, which is opt-in.
#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = IdleStop)]
pub struct IdleStopConfig {
    #[option(default = false)]
    pub enabled: Option<bool>,
}

/// Default cleanup choice when an agent marks its workspace done.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Done)]
pub struct DoneConfig {
    #[option(default = true)]
    pub cleanup: Option<bool>,
}

/// Whether agent reviews of PRs and issues post their findings to the forge.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Review)]
pub struct ReviewConfig {
    #[option(default = true)]
    pub post: Option<bool>,
}

/// Whether `shoal ready` takes marked PRs out of draft.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Ready)]
pub struct ReadyConfig {
    #[option(default = true)]
    pub undraft: Option<bool>,
}

/// Whether `shoal ready --undo` returns PRs whose marks it withdraws to draft.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Unready)]
pub struct UnreadyConfig {
    #[option(default = false)]
    pub draft: Option<bool>,
}

/// Whether `shoal pr open` opens new PRs as drafts, and the labels a new PR
/// needs.
#[derive(Debug, Default, Clone, Serialize, Deserialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Pr)]
pub struct PrConfig {
    #[option(default = false)]
    pub draft: Option<bool>,
    /// Entries a new PR's labels must each start with; an empty list turns
    /// inherited entries off.
    #[option(default = Vec::new())]
    pub required_labels: Option<Vec<String>>,
}

/// The labels a new issue needs, whether agents may open issues in other
/// repositories, and the labels that say whether an issue's plan is settled.
#[derive(Debug, Default, Clone, Serialize, Deserialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Issue)]
pub struct IssueConfig {
    /// Entries a new issue's labels must each start with; an empty list turns
    /// inherited entries off.
    #[option(default = Vec::new())]
    pub required_labels: Option<Vec<String>>,
    /// Whether scoped callers may open issues in other repositories.
    #[option(default = OtherRepositories::default())]
    pub other_repositories: Option<OtherRepositories>,
    /// Marks an issue that needs a plan before anyone implements it.
    pub plan_label: Option<String>,
    /// Marks an issue whose body holds a settled plan.
    pub planned_label: Option<String>,
}

/// Whether `shoal forge api` sends an agent's requests that are not reads.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Api)]
pub struct ApiConfig {
    #[option(default = false)]
    pub agent_writes: Option<bool>,
}

/// Whether `shoal land` pushes the default branch to its upstream afterwards,
/// and whether a scoped landing waits for the user's approval.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Land)]
pub struct LandConfig {
    #[option(default = false)]
    pub push: Option<bool>,
    #[option(default = true)]
    pub requires_approval: Option<bool>,
}

states!(
    #[derive(Default)]
    /// Whether a scoped caller's forge write happens at once or waits for the
    /// user's approval.
    Opening {
        #[default]
        Direct => "direct",
        Propose => "propose",
    }
);

states!(
    #[derive(Default)]
    /// Whether a scoped caller may open an issue in another repository than
    /// its workspace's, at once or after the user's approval.
    OtherRepositories {
        Deny => "deny",
        #[default]
        Propose => "propose",
        Direct => "direct",
    }
);

/// Which agent forge writes wait for the user's approval.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Approvals)]
pub struct ApprovalsConfig {
    #[option(default = Opening::default())]
    pub issue_open: Option<Opening>,
    #[option(default = Opening::default())]
    pub pr_open: Option<Opening>,
}

#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = PrCleanup)]
pub struct PrCleanupConfig {
    #[option(default = true)]
    pub enabled: Option<bool>,
}

#[cfg(test)]
mod tests;

/// Happy sessions are an optional integration, off until enabled.
#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Happy)]
pub struct HappyConfig {
    #[option(default = false)]
    pub enabled: Option<bool>,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Herdr)]
pub struct HerdrConfig {
    #[option(default = true)]
    pub enabled: Option<bool>,
    pub tab_name: Option<String>,
    #[option(default = true)]
    pub new_tab: Option<bool>,
    /// The workspace Shoal opens tabs in when it has none of its own;
    /// `{repo}` is the repository's name.
    #[option(default = "{repo}".into())]
    pub workspace_name: Option<String>,
    /// Overrides the top-level `close_when_done` for Herdr tabs.
    pub close_when_done: Option<bool>,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Rex)]
pub struct RexConfig {
    #[option(default = true)]
    pub enabled: Option<bool>,
    #[option(default = true)]
    pub new_window: Option<bool>,
    #[option(default = RexPlacement::default())]
    pub placement: Option<RexPlacement>,
    pub window_name: Option<String>,
    /// The session Shoal opens windows in when it has none of its own;
    /// `{repo}` is the repository's name.
    #[option(default = "{repo}".into())]
    pub session_name: Option<String>,
    /// Overrides the top-level `close_when_done` for Rex windows.
    pub close_when_done: Option<bool>,
}

states!(
    #[derive(Default)]
    /// Where Rex opens a place beside the caller's block: a new window, or a
    /// split of that block.
    RexPlacement {
        #[default]
        Window => "window",
        Split => "split",
    }
);

/// The terminal host for launches from a terminal no host serves, by the
/// host's name: any host but the plain terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "HostName", into = "HostName")]
pub struct DefaultHost(HostName);

impl Default for DefaultHost {
    fn default() -> Self {
        Self(HostName::Zmx)
    }
}

impl TryFrom<HostName> for DefaultHost {
    type Error = String;

    fn try_from(host: HostName) -> Result<Self, String> {
        if host == HostName::Terminal {
            return Err(format!(
                "default_host must name a terminal host other than {host}"
            ));
        }
        Ok(Self(host))
    }
}

impl From<DefaultHost> for HostName {
    fn from(default: DefaultHost) -> Self {
        default.0
    }
}

impl DefaultHost {
    fn resolve<C>(self, _config: &C) -> Result<HostName> {
        Ok(self.0)
    }
}
