use crate::agent::Agent;
use crate::config::options::Options;
use crate::sessions::HostName;
use crate::state::states;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

/// The repository-owned configuration layers before they are resolved. Keeping
/// them separate lets callers explain where an effective value came from.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct ConfigLayers {
    pub worktree_file: RepoConfig,
    /// The file in the repository's own directory, outside every Git tree.
    pub local_config: RepoConfig,
}

/// Display uses human-readable layer labels rather than the snake_case JSON names.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConfigLayer {
    BuiltInDefault,
    GlobalConfig,
    WorktreeFile,
    LocalConfig,
}

impl ConfigLayer {
    /// Whether the repository sets this layer, rather than the machine.
    pub fn is_repository(self) -> bool {
        matches!(self, Self::WorktreeFile | Self::LocalConfig)
    }
}

impl std::fmt::Display for ConfigLayer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::BuiltInDefault => "built-in default",
            Self::GlobalConfig => "global config",
            Self::WorktreeFile => "worktree file",
            Self::LocalConfig => "local config",
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
    /// Workspace shortcuts that are not agents: `shoal <name> [workspace]`.
    #[option(named)]
    pub commands: crate::config::named_commands::Commands,
    #[option(named_sections, default = crate::config::agents::built_in())]
    pub agents: BTreeMap<String, crate::config::agents::AgentToolConfig>,
    /// The Git profile new worktrees use.
    pub git_profile: Option<String>,
    #[option(section)]
    pub agent: AgentConfig,
    #[option(section)]
    pub terminal: TerminalConfig,
    #[option(section)]
    pub setup: SetupConfig,
    #[option(section)]
    pub hooks: HooksConfig,
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
    /// Machine-specific, so the global config or local config sets it, never
    /// the worktree file.
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
    pub idle_stop: IdleStopConfig,
    #[option(section)]
    pub pr: PrConfig,
    #[option(section)]
    pub issue: IssueConfig,
    #[option(section)]
    pub review: ReviewConfig,
    #[option(section)]
    pub messages: MessagesConfig,
    #[option(section)]
    pub land: LandConfig,
    #[option(section)]
    pub permissions: PermissionsConfig,
}

/// What every agent of a repository starts with.
#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = AgentSettings)]
pub struct AgentConfig {
    /// Agent an issue workspace starts when `--agent` is omitted.
    pub default: Option<Agent>,
    /// Agents of this repository running at once; by default only the
    /// machine limit applies. Use `none` for unlimited.
    #[option(default = crate::config::AgentLimit::Dynamic)]
    pub max_running: Option<crate::config::AgentLimit>,
    /// Directories every agent and chat gets as other working directories.
    #[option(default = vec!["/tmp".into()])]
    pub dirs: Option<Vec<String>>,
    /// The Git profile of tracked agents' Git commands.
    pub git_profile: Option<String>,
    #[option(section)]
    pub forge_clis: crate::agent_auth::ForgeClis,
    #[option(section)]
    pub templates: TemplatesConfig,
}

/// Instructions agents and chats receive after Shoal's own.
#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Templates)]
pub struct TemplatesConfig {
    #[option(template)]
    pub worker: Option<String>,
    #[option(template, default = crate::config::templates::CHAT_DEFAULT.into())]
    pub chat: Option<String>,
    /// A local review's style, after its fixed instructions.
    #[option(template, default = crate::config::templates::review_base().into())]
    pub review: Option<String>,
}

/// Where and how Shoal opens places for a workspace.
#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Terminal)]
pub struct TerminalConfig {
    /// The terminal host for launches from a terminal no host serves.
    #[option(default = DefaultHost::default(), resolve = DefaultHost::resolve -> HostName)]
    pub default_host: Option<DefaultHost>,
    /// Whether new tabs and sessions take the terminal; unset follows the launch.
    pub focus: Option<bool>,
    /// Whether a place Shoal opened for a workspace closes once it is done.
    #[option(default = true)]
    pub close_when_done: Option<bool>,
    #[option(section)]
    pub herdr: HerdrConfig,
    #[option(section)]
    pub rex: RexConfig,
    #[option(section)]
    pub happy: HappyConfig,
}

/// How a new worktree is prepared.
#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Setup)]
pub struct SetupConfig {
    /// The tracked setup executable, which gates readiness.
    #[option(repository_only)]
    pub command: Option<String>,
    /// Copy the main checkout's ignored files into the worktree before setup.
    #[option(default = true)]
    pub copy_ignored: Option<bool>,
    /// Build tools whose usual setup the repository's workspaces share; an
    /// empty list turns off presets a lower layer names. Unset when no layer
    /// names presets.
    pub presets: Option<Vec<crate::presets::Preset>>,
    #[option(section, repository_only)]
    pub seed: Seed,
}

/// Untracked lifecycle executables, by the event they follow or precede.
#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Hooks)]
pub struct HooksConfig {
    pub pre_setup: Option<String>,
    /// Runs untracked after the workspace is ready, e.g. to open a tmux session.
    pub post_setup: Option<String>,
    /// Runs untracked before the worktree is removed, e.g. to close that session.
    pub pre_remove: Option<String>,
    pub post_remove: Option<String>,
    pub post_done: Option<String>,
    pub post_ready: Option<String>,
    pub post_agent_exit: Option<String>,
    pub post_resource_acquire: Option<String>,
    pub pre_resource_release: Option<String>,
}

fn preset_environment(
    env: BTreeMap<String, String>,
    config: &RepoConfig,
) -> Result<BTreeMap<String, String>> {
    let presets = config.setup.presets.as_deref().unwrap_or_default();
    Ok(crate::presets::environment(presets, env))
}

fn preset_caches(
    caches: BTreeMap<String, Cache>,
    config: &RepoConfig,
) -> Result<BTreeMap<String, Cache>> {
    let presets = config.setup.presets.as_deref().unwrap_or_default();
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
    pub command: Option<String>,
    /// Worktree-relative build outputs cloned into each new workspace.
    #[option(list)]
    pub paths: Vec<String>,
}

impl Seed {
    fn validate(&self) -> Result<()> {
        // The seed runs unattended from the default branch, so its command
        // is the repository's own file, like its outputs.
        if let Some(command) = &self.command {
            ensure!(
                inside_worktree(command),
                "setup.seed.command must be a relative path inside the worktree: {command}"
            );
        }
        for (index, path) in self.paths.iter().enumerate() {
            ensure!(
                inside_worktree(path),
                "setup.seed.paths entries must be relative paths inside the worktree: {path}"
            );
            // Each output is cloned whole, so one cannot contain another.
            for other in &self.paths[..index] {
                ensure!(
                    !Path::new(path).starts_with(other) && !Path::new(other).starts_with(path),
                    "setup.seed.paths entries overlap: {other} and {path}"
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

pub fn parse(text: &str) -> Result<RepoConfig> {
    let config = crate::config::parse::repository(text)?;
    config.validate()?;
    Ok(config)
}

/// The directory in a repository's own directory that holds its local
/// config and the scripts it names, beside the workspaces and `.checkout`.
pub const LOCAL_DIR: &str = ".shoal";

/// `<repository directory>/.shoal/config.toml`.
pub fn local_config_path(repository_dir: &Path) -> PathBuf {
    repository_dir.join(LOCAL_DIR).join("config.toml")
}

/// The local config of the repository whose directory is `repository_dir`,
/// read in place, with the templates beside it. Relative hook paths and
/// `setup.command` name scripts in its directory, so they become absolute.
pub fn load_local(repository_dir: &Path) -> Result<RepoConfig> {
    let path = local_config_path(repository_dir);
    let directory = repository_dir.join(LOCAL_DIR);
    let text =
        crate::fsutil::read_optional(&path).with_context(|| format!("read {}", path.display()))?;
    let mut config = match text {
        Some(text) => parse(&text).with_context(|| format!("parse {}", path.display()))?,
        None => RepoConfig::default(),
    };
    resolve_script_paths(&mut config, &directory);
    read_templates(&mut config, &directory)?;
    Ok(config)
}

/// Make each relative hook path and `setup.command` name a script in
/// `directory`.
fn resolve_script_paths(config: &mut RepoConfig, directory: &Path) {
    for &kind in crate::hooks::HookKind::ALL {
        if let Some(command) = kind.repository_command_mut(config) {
            *command = directory.join(&*command).to_string_lossy().into_owned();
        }
    }
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
                "{}: [secrets] belongs in the global or local config",
                path.display()
            );
            config
        }
        None => RepoConfig::default(),
    };
    read_templates(&mut config, workspace_dir)?;
    Ok(config)
}

/// Fill omitted inline templates from the files in `directory`.
fn read_templates(config: &mut RepoConfig, directory: &Path) -> Result<()> {
    let templates = &mut config.agent.templates;
    if templates.worker.is_none() {
        templates.worker =
            crate::config::templates::read(directory, crate::config::templates::WORKER_FILE)?;
    }
    if templates.chat.is_none() {
        templates.chat =
            crate::config::templates::read(directory, crate::config::templates::CHAT_FILE)?;
    }
    if templates.review.is_none() {
        templates.review =
            crate::config::templates::read(directory, crate::config::templates::REVIEW_FILE)?;
    }
    Ok(())
}

impl RepoConfig {
    /// Warn about a worker template that repeats the built-in guidance.
    pub fn normalize(&self) -> Vec<&'static str> {
        let mut warnings = Vec::new();
        if self
            .agent
            .templates
            .worker
            .as_deref()
            .is_some_and(crate::config::templates::repeats_guidance)
        {
            warnings.push(
                "agent.templates.worker repeats Shoal's built-in guidance, which every launch \
                 already receives; keep only your additions",
            );
        }
        warnings
    }

    /// Each option's value on its own; layers combine into settings that
    /// [`Stack::resolve`](crate::config::resolve::Stack::resolve) checks again.
    pub fn validate(&self) -> Result<()> {
        crate::config::named_commands::validate(&self.commands)?;
        crate::config::agents::validate(&self.agents, &self.commands)?;
        self.agent.forge_clis.validate()?;
        for name in [&self.git_profile, &self.agent.git_profile]
            .into_iter()
            .flatten()
        {
            crate::validate::name("git profile", name)?;
        }
        for &kind in crate::hooks::HookKind::ALL {
            kind.validate(kind.repository_command(self))?;
        }
        self.cleanup.validate()?;
        self.messages.validate()?;
        crate::config::validate_port_range(self.ports.start, self.ports.end)?;
        for (name, definition) in &self.ports.definitions {
            crate::validate::lowercase_name("port", name)
                .with_context(|| format!("invalid configured port name: {name}"))?;
            ensure!(
                definition.port != Some(0),
                "configured port {name} cannot use port zero"
            );
        }
        crate::daemon::resources::definitions(&self.resources, &self.resource_pools)?;
        crate::env::validate_configured_environment(&self.env)?;
        crate::config::validate_agent_dirs(self.agent.dirs.as_deref().unwrap_or_default())?;
        crate::caches::validate(&self.caches)?;
        crate::demos::validate(&self.demos)?;
        crate::presets::validate(self.setup.presets.as_deref().unwrap_or_default())?;
        crate::secrets::validate(&self.secrets)?;
        self.setup.seed.validate()?;
        Ok(())
    }
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
    #[option(section)]
    pub done: CleanupDoneConfig,
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

/// Who runs local reviews and whether PR reviews post their findings.
#[derive(Debug, Default, Clone, Serialize, Deserialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Review)]
pub struct ReviewConfig {
    #[option(default = true)]
    pub post: Option<bool>,
    /// The agent that reviews; unset, `agent.default`.
    pub agent: Option<Agent>,
    /// The agent that reviews once more when the first reviewer fails for a
    /// reason other than running too long.
    pub fallback_agent: Option<Agent>,
    /// Whether a workspace process opening a new PR needs a finished review
    /// of HEAD with every finding resolved.
    #[option(default = false)]
    pub require_for_pr: Option<bool>,
    /// Whether a workspace process marking work ready needs one.
    #[option(default = false)]
    pub require_for_ready: Option<bool>,
}

/// How agent messages that arrive in quick succession are held to reach the
/// agent together.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Messages)]
pub struct MessagesConfig {
    #[option(default = 5)]
    pub settle_seconds: Option<u64>,
}

impl MessagesConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        if let Some(seconds) = self.settle_seconds {
            anyhow::ensure!(seconds <= 60, "messages.settle_seconds must be at most 60");
        }
        Ok(())
    }
}

/// Whether new PRs open as drafts and `shoal ready` moves them, and the labels
/// a new PR needs.
#[derive(Debug, Default, Clone, Serialize, Deserialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Pr)]
pub struct PrConfig {
    #[option(default = false)]
    pub draft: Option<bool>,
    /// Whether `shoal ready` takes marked PRs out of draft.
    #[option(default = true)]
    pub undraft_on_ready: Option<bool>,
    /// Whether `shoal ready --undo` returns PRs whose marks it withdraws to draft.
    #[option(default = false)]
    pub draft_on_unready: Option<bool>,
    /// Entries a new PR's labels must each start with; an empty list turns
    /// inherited entries off.
    #[option(default = Vec::new())]
    pub required_labels: Option<Vec<String>>,
}

/// The labels a new issue needs and the labels that say whether an issue's
/// plan is settled.
#[derive(Debug, Default, Clone, Serialize, Deserialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Issue)]
pub struct IssueConfig {
    /// Entries a new issue's labels must each start with; an empty list turns
    /// inherited entries off.
    #[option(default = Vec::new())]
    pub required_labels: Option<Vec<String>>,
    /// Marks an issue that needs a plan before anyone implements it.
    pub plan_label: Option<String>,
    /// Marks an issue whose body holds a settled plan.
    pub planned_label: Option<String>,
}

/// Whether `shoal land` pushes the default branch to its upstream afterwards.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Land)]
pub struct LandConfig {
    #[option(default = false)]
    pub push: Option<bool>,
}

states!(
    #[derive(Default)]
    /// Whether a scoped caller's action happens at once or waits for the
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

states!(
    #[derive(Default)]
    /// Whether a scoped caller may send forge API requests that are not reads.
    ApiWrites {
        #[default]
        Deny => "deny",
        Direct => "direct",
    }
);

/// What a scoped agent may do at once, after the user's approval, or not at all.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Permissions)]
pub struct PermissionsConfig {
    #[option(default = Opening::Direct)]
    pub issue_open: Option<Opening>,
    #[option(default = Opening::Direct)]
    pub pr_open: Option<Opening>,
    /// Opening an issue in another repository than the workspace's.
    #[option(default = OtherRepositories::Propose)]
    pub issue_other_repositories: Option<OtherRepositories>,
    /// Landing a branch with `shoal land`.
    #[option(default = Opening::Propose)]
    pub land: Option<Opening>,
    /// Forge API requests through `shoal forge api` that are not reads.
    #[option(default = ApiWrites::Deny)]
    pub api_writes: Option<ApiWrites>,
}

/// Whether `shoal done` cleans up by default; false keeps the workspace for review.
#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = CleanupDone)]
pub struct CleanupDoneConfig {
    #[option(default = true)]
    pub enabled: Option<bool>,
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
