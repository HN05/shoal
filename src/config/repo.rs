use crate::cli::{Agent, CodexMode};
use crate::state::states;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

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

impl ConfigLayers {
    pub fn resolve(self) -> RepoConfig {
        self.saved_repository_config.over(self.worktree_file)
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
    pub issue_template: Option<String>,
    pub agent_template: Option<String>,
    pub agent_auth: crate::agent_auth::Config,
    pub git_profile: Option<String>,
    /// Agent `shoal issue` starts when `--agent` is omitted.
    pub default_agent: Option<Agent>,
    pub codex: Codex,
    pub setup_cmd: Option<String>,
    pub pre_setup_cmd: Option<String>,
    pub post_remove_cmd: Option<String>,
    pub post_resource_acquire_cmd: Option<String>,
    pub pre_resource_release_cmd: Option<String>,
    /// Runs untracked after the workspace is ready, e.g. to open a tmux session.
    pub post_setup_cmd: Option<String>,
    /// Runs untracked before the worktree is removed, e.g. to close that session.
    pub pre_remove_cmd: Option<String>,
    pub ports: PortDefaults,
    pub resources: BTreeMap<String, crate::daemon::resources::ResourceConfig>,
    pub resource_pools: BTreeMap<String, crate::daemon::resources::PoolConfig>,
    pub simulators: SimulatorPreferences,
    pub auto_cleanup: AutoCleanup,
    pub pr_cleanup: PrCleanup,
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
    pub approval_lifetime: crate::daemon::access::Lifetime,
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
            parse(&text).with_context(|| format!("parse {}", path.display()))?
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
    Ok(config)
}

pub fn parse(text: &str) -> Result<RepoConfig> {
    let config: RepoConfig = toml::from_str(text)?;
    crate::config::named_commands::validate(&config.commands)?;
    config.agent_auth.validate()?;
    if let Some(name) = &config.git_profile {
        crate::validate::name("git profile", name)?;
    }
    for (key, command) in [
        ("setup_cmd", &config.setup_cmd),
        ("pre_setup_cmd", &config.pre_setup_cmd),
        ("post_remove_cmd", &config.post_remove_cmd),
        (
            "post_resource_acquire_cmd",
            &config.post_resource_acquire_cmd,
        ),
        ("pre_resource_release_cmd", &config.pre_resource_release_cmd),
        ("post_setup_cmd", &config.post_setup_cmd),
        ("pre_remove_cmd", &config.pre_remove_cmd),
    ] {
        if let Some(command) = command {
            ensure!(
                !command.trim().is_empty() && !command.contains('\0'),
                "{key} must be a nonempty executable path"
            );
        }
    }
    if let Some(minutes) = config.auto_cleanup.idle_minutes {
        crate::config::validate_idle_minutes(minutes)?;
    }
    ensure!(
        config.ports.start != Some(0),
        "ports.start must be between 1 and 65535"
    );
    if let (Some(start), Some(end)) = (config.ports.start, config.ports.end) {
        ensure!(
            start <= end,
            "ports.start/end must specify a nonempty range"
        );
    }
    for (name, definition) in &config.ports.definitions {
        crate::validate::lowercase_name("port", name)
            .with_context(|| format!("invalid configured port name: {name}"))?;
        ensure!(
            definition.port != Some(0),
            "configured port {name} cannot use port zero"
        );
    }
    crate::daemon::resources::definitions(&config.resources, &config.resource_pools)?;
    Ok(config)
}

impl RepoConfig {
    /// This config layered over `base`: an option set here wins, an omitted
    /// one falls through, and a named port, resource or pool replaces the
    /// one below it whole.
    pub fn over(self, mut base: Self) -> Self {
        base.ports.definitions.extend(self.ports.definitions);
        base.resources.extend(self.resources);
        base.commands.extend(self.commands);
        base.resource_pools.extend(self.resource_pools);
        Self {
            commands: base.commands,
            issue_template: self.issue_template.or(base.issue_template),
            agent_template: self.agent_template.or(base.agent_template),
            agent_auth: self.agent_auth.over(base.agent_auth),
            git_profile: self.git_profile.or(base.git_profile),
            default_agent: self.default_agent.or(base.default_agent),
            codex: Codex {
                default_mode: self.codex.default_mode.or(base.codex.default_mode),
            },
            setup_cmd: self.setup_cmd.or(base.setup_cmd),
            pre_setup_cmd: self.pre_setup_cmd.or(base.pre_setup_cmd),
            post_remove_cmd: self.post_remove_cmd.or(base.post_remove_cmd),
            post_resource_acquire_cmd: self
                .post_resource_acquire_cmd
                .or(base.post_resource_acquire_cmd),
            pre_resource_release_cmd: self
                .pre_resource_release_cmd
                .or(base.pre_resource_release_cmd),
            post_setup_cmd: self.post_setup_cmd.or(base.post_setup_cmd),
            pre_remove_cmd: self.pre_remove_cmd.or(base.pre_remove_cmd),
            ports: PortDefaults {
                on_conflict: self.ports.on_conflict.or(base.ports.on_conflict),
                start: self.ports.start.or(base.ports.start),
                end: self.ports.end.or(base.ports.end),
                definitions: base.ports.definitions,
            },
            resources: base.resources,
            resource_pools: base.resource_pools,
            simulators: SimulatorPreferences {
                preferred: if self.simulators.preferred.is_empty() {
                    base.simulators.preferred
                } else {
                    self.simulators.preferred
                },
                requires_approval: self
                    .simulators
                    .requires_approval
                    .or(base.simulators.requires_approval),
                approval_lifetime: self
                    .simulators
                    .approval_lifetime
                    .or(base.simulators.approval_lifetime),
            },
            auto_cleanup: AutoCleanup {
                enabled: self.auto_cleanup.enabled.or(base.auto_cleanup.enabled),
                idle_minutes: self
                    .auto_cleanup
                    .idle_minutes
                    .or(base.auto_cleanup.idle_minutes),
            },
            pr_cleanup: PrCleanup {
                enabled: self.pr_cleanup.enabled.or(base.pr_cleanup.enabled),
            },
        }
    }

    /// Hook executables resolved against the worktree, like `setup_cmd`.
    pub fn hooks(&self, worktree: &Path) -> Hooks {
        let resolve = |command: &Option<String>| command.as_ref().map(|c| worktree.join(c));
        Hooks {
            post_setup_cmd: resolve(&self.post_setup_cmd),
            pre_remove_cmd: resolve(&self.pre_remove_cmd),
        }
    }
}

/// The effective, resolved lifecycle hooks of one workspace.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Hooks {
    pub post_setup_cmd: Option<PathBuf>,
    pub pre_remove_cmd: Option<PathBuf>,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SimulatorPreferences {
    pub requires_approval: Option<bool>,
    pub approval_lifetime: Option<crate::daemon::access::Lifetime>,
    pub preferred: Vec<String>,
}

/// Repository value for the global `[codex]`; `None` keeps the layer below.
#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Codex {
    pub default_mode: Option<CodexMode>,
}

/// Repository values for the global `[auto_cleanup]`.
#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AutoCleanup {
    pub enabled: Option<bool>,
    pub idle_minutes: Option<u64>,
}

/// Repository value for the global `[pr_cleanup]`.
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
             [auto_cleanup]\nenabled = false\nidle_minutes = 30\n",
        )
        .unwrap();
        let local = parse(
            "setup_cmd = 'local/setup'\n[codex]\ndefault_mode = 'app'\n\
             [ports]\nend = 3050\n[ports.web]\nenv = 'LOCAL_PORT'\n\
             [resources.signing]\ncapacity = 2\n[auto_cleanup]\nenabled = true\n",
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
        assert_eq!(config.auto_cleanup.enabled, Some(true));
        assert_eq!(config.auto_cleanup.idle_minutes, Some(30));
        assert_eq!(config.pr_cleanup.enabled, None);
        assert!(parse("[auto_cleanup]\nidle_minutes = 0\n").is_err());
        assert!(parse("default_agent = 'happy'\n").is_err());
        assert!(parse("[codex]\ndefault_mode = 'desktop'\n").is_err());
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
            Some(Agent::Happy(crate::happy::HappyAgent::Codex))
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
        for key in [
            "pre_setup_cmd",
            "post_remove_cmd",
            "post_resource_acquire_cmd",
            "pre_resource_release_cmd",
        ] {
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

    #[test]
    fn hooks_resolve_relative_paths_against_the_worktree() {
        let config =
            parse("post_setup_cmd = 'scripts/attach.sh'\npre_remove_cmd = '/opt/detach'\n")
                .unwrap();
        let hooks = config.hooks(Path::new("/work/tree"));
        assert_eq!(
            hooks.post_setup_cmd.as_deref(),
            Some(Path::new("/work/tree/scripts/attach.sh"))
        );
        assert_eq!(
            hooks.pre_remove_cmd.as_deref(),
            Some(Path::new("/opt/detach"))
        );
        assert!(
            parse("")
                .unwrap()
                .hooks(Path::new("/w"))
                .post_setup_cmd
                .is_none()
        );
    }
}
