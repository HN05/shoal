use crate::agent::{Agent, AgentMode};
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

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RepoConfig {
    pub commands: crate::config::named_commands::Commands,
    pub agent_resume: crate::config::named_commands::Commands,
    /// Retired with issue templates; kept so configs written by earlier
    /// versions still load and `config show` can say it is unused.
    #[serde(rename = "issue_template")]
    pub retired_issue_template: Option<String>,
    pub worker_template: Option<String>,
    /// What `worker_template` was called; still read, with a rename warning.
    #[serde(rename = "agent_template")]
    pub renamed_agent_template: Option<String>,
    pub chat_template: Option<String>,
    pub agent_auth: crate::agent_auth::Config,
    pub git_profile: Option<String>,
    /// Agent an issue workspace starts when `--agent` is omitted.
    pub default_agent: Option<Agent>,
    /// Agents of this repository running at once; defaults to the host
    /// capacity and `shoal add` queues past it. Use `none` for unlimited.
    pub max_agents: Option<crate::config::AgentLimit>,
    /// Whether new tabs and sessions take the terminal; unset follows the launch.
    pub focus: Option<bool>,
    /// Whether a place Shoal opened for a workspace closes once it is done.
    pub close_when_done: Option<bool>,
    pub claude: Shortcut,
    pub codex: Shortcut,
    pub herdr: Herdr,
    pub happy: Happy,
    /// Copy the main checkout's ignored files into the worktree before setup.
    pub copy_ignored: Option<bool>,
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
    pub ports: PortDefaults,
    pub env: BTreeMap<String, String>,
    pub caches: BTreeMap<String, Cache>,
    /// Commands kept running for the user to try, by name.
    pub demos: BTreeMap<String, crate::demos::Demo>,
    /// Build tools whose usual setup the repository's workspaces share; an
    /// empty list turns off presets a lower layer names.
    pub presets: Option<Vec<crate::presets::Preset>>,
    /// Machine-specific, so the global config or saved repository config sets
    /// it, never the worktree file.
    pub secrets: crate::secrets::Secrets,
    pub resources: BTreeMap<String, crate::daemon::resources::ResourceConfig>,
    pub resource_pools: BTreeMap<String, crate::daemon::resources::PoolConfig>,
    pub simulators: SimulatorPreferences,
    pub cleanup: Cleanup,
    /// Former names of `[cleanup.auto]` and `[cleanup.pr]`; parsing folds them
    /// into `cleanup` so existing configs load.
    #[serde(skip_serializing)]
    pub auto_cleanup: AutoCleanup,
    #[serde(skip_serializing)]
    pub pr_cleanup: PrCleanup,
    pub done: Done,
    pub review: Review,
    pub ready: Ready,
    pub unready: Unready,
    pub pr: Pr,
    pub issue: Issue,
    pub land: Land,
    pub approvals: Approvals,
    pub api: Api,
    pub seed: Seed,
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
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Seed {
    /// Executable in the seed worktree that builds the default branch.
    pub cmd: Option<String>,
    /// Worktree-relative build outputs cloned into each new workspace.
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

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct PortDefaults {
    pub on_conflict: Option<ConflictPolicy>,
    /// Bounds of the automatic range; each falls back to the global `[ports]`.
    pub start: Option<u16>,
    pub end: Option<u16>,
    #[serde(flatten)]
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
    if config.retired_issue_template.is_none() {
        config.retired_issue_template = crate::config::templates::read(
            workspace_dir,
            crate::config::templates::RETIRED_ISSUE_FILE,
        )?;
    }
    crate::config::templates::read_worker(
        workspace_dir,
        &mut config.worker_template,
        &mut config.renamed_agent_template,
    )?;
    if config.chat_template.is_none() {
        config.chat_template =
            crate::config::templates::read(workspace_dir, crate::config::templates::CHAT_FILE)?;
    }
    Ok(config)
}

impl RepoConfig {
    /// Move settings set under a former name to their current one, and warn
    /// about each, about settings Shoal no longer reads, and about a worker
    /// template that repeats the built-in guidance.
    pub fn normalize(&mut self) -> Vec<&'static str> {
        let mut warnings = Vec::new();
        if let Some(template) = self.renamed_agent_template.take() {
            self.worker_template = Some(template);
            warnings.push(
                "agent_template and agent-template.md are now named \
                 worker_template and worker-template.md",
            );
        }
        if self
            .retired_issue_template
            .as_deref()
            .is_some_and(crate::config::templates::is_custom_issue_template)
        {
            warnings.push(
                "issue_template and issue-template.md are no longer used; \
                 move their instructions to worker_template",
            );
        }
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
    let mut config: RepoConfig = toml::from_str(text)?;
    config
        .cleanup
        .fold_legacy(&mut config.auto_cleanup, &mut config.pr_cleanup);
    crate::config::named_commands::validate(&config.commands)?;
    crate::config::named_commands::validate(&config.agent_resume)?;
    crate::config::templates::ensure_one_worker(
        &config.worker_template,
        &config.renamed_agent_template,
    )?;
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
    crate::caches::validate(&config.caches)?;
    crate::demos::validate(&config.demos)?;
    crate::presets::validate(config.presets.as_deref().unwrap_or_default())?;
    crate::secrets::validate(&config.secrets)?;
    config.seed.validate()?;
    Ok(config)
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SimulatorPreferences {
    pub requires_approval: Option<bool>,
    pub approval_lifetime: Option<crate::daemon::approvals::Lifetime>,
    pub preferred: Vec<String>,
}

/// Repository value for the global `[claude]` or `[codex]`; `None` keeps the
/// layer below.
#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Shortcut {
    pub default_mode: Option<AgentMode>,
}

/// Repository values for the global `[cleanup]`.
#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Cleanup {
    pub grace_minutes: Option<u64>,
    pub auto: AutoCleanup,
    pub pr: PrCleanup,
    pub merged: MergedCleanup,
    pub restore: Restore,
}

impl Cleanup {
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

    /// Take the values the former `[auto_cleanup]` and `[pr_cleanup]` tables
    /// set where `[cleanup]` leaves them unset, emptying those tables.
    pub fn fold_legacy(&mut self, auto: &mut AutoCleanup, pr: &mut PrCleanup) {
        let (auto, pr) = (std::mem::take(auto), std::mem::take(pr));
        self.auto.enabled = self.auto.enabled.or(auto.enabled);
        self.auto.idle_minutes = self.auto.idle_minutes.or(auto.idle_minutes);
        self.pr.enabled = self.pr.enabled.or(pr.enabled);
    }
}

/// Repository values for the global `[cleanup.auto]`.
#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AutoCleanup {
    pub enabled: Option<bool>,
    pub idle_minutes: Option<u64>,
}

/// Repository values for the global `[cleanup.merged]`.
#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct MergedCleanup {
    pub enabled: Option<bool>,
    pub idle_minutes: Option<u64>,
}

/// Repository values for the global `[cleanup.restore]`.
#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Restore {
    pub retention_days: Option<u64>,
    pub archive_max_mb: Option<u64>,
}

/// Default cleanup choice when an agent marks its workspace done.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Done {
    pub cleanup: Option<bool>,
    /// Retired with automatic completion, which merged cleanup replaced; read
    /// and ignored so configs written by earlier versions still load.
    #[serde(rename = "automatic", skip_serializing)]
    pub retired_automatic: Option<bool>,
}

/// Whether agent reviews of PRs and issues post their findings to the forge.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Review {
    pub post: Option<bool>,
}

/// Whether `shoal ready` takes marked PRs out of draft.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Ready {
    pub undraft: Option<bool>,
}

/// Whether `shoal ready --undo` returns PRs whose marks it withdraws to draft.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Unready {
    pub draft: Option<bool>,
}

/// Whether `shoal pr open` opens new PRs as drafts, and the labels a new PR
/// needs.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Pr {
    pub draft: Option<bool>,
    /// An empty list turns inherited entries off.
    pub required_labels: Option<Vec<String>>,
}

/// The labels a new issue needs, whether agents may open issues in other
/// repositories, and the labels that say whether an issue's plan is settled.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Issue {
    /// An empty list turns inherited entries off.
    pub required_labels: Option<Vec<String>>,
    pub other_repositories: Option<OtherRepositories>,
    pub plan_label: Option<String>,
    pub planned_label: Option<String>,
}

/// Whether `shoal forge api` sends an agent's requests that are not reads.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Api {
    pub agent_writes: Option<bool>,
}

/// Whether `shoal land` pushes the default branch to its upstream afterwards,
/// and whether a scoped landing waits for the user's approval.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Land {
    pub push: Option<bool>,
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
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Approvals {
    pub issue_open: Option<Opening>,
    pub pr_open: Option<Opening>,
}

/// Repository value for the global `[cleanup.pr]`.
#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PrCleanup {
    pub enabled: Option<bool>,
}

#[cfg(test)]
mod tests;

/// Happy sessions are an optional integration, off until enabled.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Happy {
    pub enabled: Option<bool>,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Herdr {
    pub enabled: Option<bool>,
    pub tab_name: Option<String>,
    pub new_tab: Option<bool>,
    pub close_when_done: Option<bool>,
}
