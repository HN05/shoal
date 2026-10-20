use crate::agent::{Agent, CodexMode};
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
    pub issue_template: Option<String>,
    pub agent_template: Option<String>,
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
    pub codex: Codex,
    pub herdr: Herdr,
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
    pub land: Land,
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
    if config.issue_template.is_none() {
        config.issue_template =
            crate::config::templates::read(workspace_dir, crate::config::templates::ISSUE_FILE)?;
    }
    if config.agent_template.is_none() {
        config.agent_template =
            crate::config::templates::read(workspace_dir, crate::config::templates::AGENT_FILE)?;
    }
    if config.chat_template.is_none() {
        config.chat_template =
            crate::config::templates::read(workspace_dir, crate::config::templates::CHAT_FILE)?;
    }
    Ok(config)
}

pub fn parse(text: &str) -> Result<RepoConfig> {
    let mut config: RepoConfig = toml::from_str(text)?;
    config
        .cleanup
        .fold_legacy(&mut config.auto_cleanup, &mut config.pr_cleanup);
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
    crate::caches::validate(&config.caches)?;
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

/// Repository value for the global `[codex]`; `None` keeps the layer below.
#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Codex {
    pub default_mode: Option<CodexMode>,
}

/// Repository values for the global `[cleanup]`.
#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Cleanup {
    pub auto: AutoCleanup,
    pub pr: PrCleanup,
    pub merged: MergedCleanup,
    pub restore: Restore,
}

impl Cleanup {
    pub fn validate(&self) -> anyhow::Result<()> {
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

/// Whether `shoal unready` returns PRs whose marks it withdraws to draft.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Unready {
    pub draft: Option<bool>,
}

/// Whether `shoal pr open` opens new PRs as drafts.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Pr {
    pub draft: Option<bool>,
}

/// Whether `shoal land` pushes the default branch to its upstream afterwards,
/// and whether a scoped landing waits for the user's approval.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Land {
    pub push: Option<bool>,
    pub requires_approval: Option<bool>,
}

/// Repository value for the global `[cleanup.pr]`.
#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PrCleanup {
    pub enabled: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_cmd_requires_a_nonempty_path() {
        assert!(parse("").unwrap().setup_cmd.is_none());
        assert_eq!(
            parse("setup_cmd = 'scripts/setup.sh'")
                .unwrap()
                .setup_cmd
                .as_deref(),
            Some("scripts/setup.sh")
        );
        for text in [
            "setup_cmd = ''",
            "setup_cmd = '  '",
            "setup_cmd = []",
            r#"setup_cmd = "a\u0000b""#,
            "post_setup_cmd = ''",
            "pre_remove_cmd = ' '",
        ] {
            assert!(parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn layering_keeps_omitted_options_and_replaces_named_entries() {
        let base = parse(
            "default_agent = 'claude'\nsetup_cmd = 'base/setup'\npost_setup_cmd = 'base/attach'\n\
             [ports]\non_conflict = 'auto'\nstart = 3000\nend = 3100\n\
             [ports.web]\nport = 3000\n[ports.api]\nport = 4000\n\
             [resources.lock]\ncapacity = 1\n[simulators]\npreferred = ['phone']\n\
             [cleanup.auto]\nenabled = false\nidle_minutes = 30\n",
        )
        .unwrap();
        let local = parse(
            "setup_cmd = 'local/setup'\n[codex]\ndefault_mode = 'app'\n\
             [ports]\nend = 3050\n[ports.web]\nenv = 'LOCAL_PORT'\n\
             [resources.signing]\ncapacity = 2\n[cleanup.auto]\nenabled = true\n",
        )
        .unwrap();
        let config = local.over(base);
        assert_eq!(config.default_agent, Some(Agent::Claude));
        assert_eq!(config.codex.default_mode, Some(CodexMode::App));
        assert_eq!(config.setup_cmd.as_deref(), Some("local/setup"));
        assert_eq!(config.post_setup_cmd.as_deref(), Some("base/attach"));
        assert!(matches!(
            config.ports.on_conflict,
            Some(ConflictPolicy::Auto)
        ));
        assert_eq!(
            (config.ports.start, config.ports.end),
            (Some(3000), Some(3050))
        );
        assert_eq!(config.ports.definitions["web"].port, None);
        assert_eq!(
            config.ports.definitions["web"].env.as_deref(),
            Some("LOCAL_PORT")
        );
        assert_eq!(config.ports.definitions["api"].port, Some(4000));
        assert_eq!(config.resources.len(), 2);
        assert_eq!(config.simulators.preferred, ["phone"]);
        assert_eq!(config.cleanup.auto.enabled, Some(true));
        assert_eq!(config.cleanup.auto.idle_minutes, Some(30));
        assert_eq!(config.cleanup.pr.enabled, None);
        assert!(parse("[cleanup.auto]\nidle_minutes = 0\n").is_err());
        // Configs written before automatic completion was retired still load.
        assert_eq!(
            parse("[done]\ncleanup = false\nautomatic = true\n")
                .unwrap()
                .done
                .cleanup,
            Some(false)
        );
        assert!(parse("default_agent = 'happy'\n").is_err());
        assert!(parse("[codex]\ndefault_mode = 'desktop'\n").is_err());
    }

    #[test]
    fn seed_paths_stay_inside_the_worktree() {
        let seed = parse("[seed]\ncmd = 'scripts/seed'\npaths = ['target', 'ios/build']\n")
            .unwrap()
            .seed;
        assert_eq!(seed.cmd.as_deref(), Some("scripts/seed"));
        assert_eq!(seed.paths, ["target", "ios/build"]);
        for text in [
            "[seed]\ncmd = ' '",
            "[seed]\ncmd = '/usr/bin/make'",
            "[seed]\ncmd = '../seed.sh'",
            "[seed]\npaths = ['']",
            "[seed]\npaths = ['/target']",
            "[seed]\npaths = ['../target']",
            "[seed]\npaths = ['./target']",
            "[seed]\npaths = ['.git']",
            "[seed]\npaths = ['target', 'target']",
            "[seed]\npaths = ['target', 'target/debug']",
            "[seed]\npaths = ['ios/build', 'ios']",
            "[seed]\npaths = ['build/../..']",
            "[seed]\ncommand = 'x'",
        ] {
            assert!(parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn former_cleanup_tables_fill_options_cleanup_leaves_unset() {
        let config = parse(
            "[auto_cleanup]\nenabled = false\nidle_minutes = 30\n[pr_cleanup]\nenabled = false\n\
             [cleanup.auto]\nenabled = true\n",
        )
        .unwrap();
        assert_eq!(config.cleanup.auto.enabled, Some(true));
        assert_eq!(config.cleanup.auto.idle_minutes, Some(30));
        assert_eq!(config.cleanup.pr.enabled, Some(false));
        assert!(parse("[auto_cleanup]\nidle_minutes = 0\n").is_err());
        let saved = toml::to_string(&config).unwrap();
        assert!(!saved.contains("auto_cleanup") && !saved.contains("pr_cleanup"));
    }

    #[test]
    fn config_serializes_in_its_own_spelling() {
        let config = parse(
            "default_agent = 'happy-codex'\n[codex]\ndefault_mode = 'app'\n[ports.web]\nport = 1\n",
        )
        .unwrap();
        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(json["default_agent"], "happy-codex");
        assert_eq!(json["codex"]["default_mode"], "app");
        let back: RepoConfig = serde_json::from_value(json).unwrap();
        assert_eq!(
            back.default_agent,
            Some(Agent::Happy(crate::agent::BuiltinAgent::Codex))
        );
        assert_eq!(back.ports.definitions["web"].port, Some(1));
        assert!(
            parse("")
                .unwrap()
                .over(parse("").unwrap())
                .ports
                .on_conflict
                .is_none()
        );
    }

    #[test]
    fn additional_hooks_validate_and_layer_paths() {
        for &kind in crate::hooks::HookKind::ALL {
            let key = kind.key();
            assert!(parse(&format!("{key} = ' '")).is_err());
            assert!(parse(&format!(r#"{key} = "a\u0000b""#)).is_err());
            let base = parse(&format!("{key} = '/base/hook'")).unwrap();
            let inherited = parse("").unwrap().over(base.clone());
            assert_eq!(serde_json::to_value(inherited).unwrap()[key], "/base/hook");
            let config = parse(&format!("{key} = 'scripts/hook'"))
                .unwrap()
                .over(base);
            assert_eq!(serde_json::to_value(config).unwrap()[key], "scripts/hook");
        }
    }
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Herdr {
    pub enabled: Option<bool>,
    pub tab_name: Option<String>,
    pub new_tab: Option<bool>,
    pub close_when_done: Option<bool>,
}
