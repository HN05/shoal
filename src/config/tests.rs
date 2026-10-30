use super::*;

const TEMPLATE: &str = include_str!("../../configs/default.toml");

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
fn agent_modes_default_to_cli_and_reject_invalid_settings() {
    use crate::agent::AgentMode;

    for text in ["", ""] {
        let config = Config::from_toml(text).unwrap();
        assert_eq!(
            config
                .repository
                .agents
                .get("claude")
                .and_then(|agent| agent.default_mode),
            None
        );
        assert_eq!(
            config
                .repository
                .agents
                .get("codex")
                .and_then(|agent| agent.default_mode),
            None
        );
        let effective = config.effective(&RepoConfig::default()).unwrap();
        assert_eq!(effective.agents["claude"].default_mode, AgentMode::Cli);
        assert_eq!(effective.agents["codex"].default_mode, AgentMode::Cli);
    }
    let config = Config::from_toml("[agents.claude]\ndefault_mode = 'app'\n").unwrap();
    assert_eq!(
        config
            .repository
            .agents
            .get("claude")
            .and_then(|agent| agent.default_mode),
        Some(AgentMode::App)
    );
    assert_eq!(
        config
            .repository
            .agents
            .get("codex")
            .and_then(|agent| agent.default_mode),
        None
    );
    let config = Config::from_toml("[agents.codex]\ndefault_mode = 'app'\n").unwrap();
    assert_eq!(
        config
            .repository
            .agents
            .get("codex")
            .and_then(|agent| agent.default_mode),
        Some(AgentMode::App)
    );
    for section in ["claude", "codex"] {
        for setting in ["default_mode = 'desktop'", "defaut_mode = 'app'"] {
            let text = format!("[agents.{section}]\n{setting}");
            assert!(Config::from_toml(&text).is_err());
        }
    }
}

#[test]
fn global_git_profile_selections_must_name_defined_profiles() {
    let paths = Paths::for_test("/home/test");
    let profiles = "[machine.git_profiles.agent]\nuser.email = 'agent@example.invalid'\n";
    for key in ["git_profile", "agent.git_profile"] {
        assert!(Config::parse(&format!("{key} = 'agent'\n{profiles}"), &paths).is_ok());
        let error = Config::parse(&format!("{key} = 'missing'\n{profiles}"), &paths)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("git profile missing is not defined"),
            "{error}"
        );
    }
    assert!(crate::config::repo::parse("[agent]\ngit_profile = 'bad name'").is_err());
}

#[test]
fn additional_hook_paths_are_validated_in_global_config() {
    let paths = Paths::for_test("/home/test");
    for &kind in crate::hooks::HookKind::ALL {
        let key = kind.key();
        if kind == crate::hooks::HookKind::Setup {
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
    assert_eq!(written.repository.agent.default, None);
    // The template writes the built-in defaults out explicitly.
    let written_effective = written.effective(&RepoConfig::default()).unwrap();
    let default_effective = defaults.effective(&RepoConfig::default()).unwrap();
    assert_eq!(
        written
            .repository
            .agents
            .get("codex")
            .and_then(|agent| agent.default_mode),
        Some(default_effective.agents["codex"].default_mode)
    );
    assert_eq!(
        written.repository.cleanup.auto.enabled,
        Some(default_effective.cleanup.auto.enabled)
    );
    assert_eq!(
        written.repository.cleanup.auto.idle_minutes,
        Some(default_effective.cleanup.auto.idle_minutes)
    );
    assert_eq!(
        written.repository.cleanup.pr.enabled,
        Some(default_effective.cleanup.pr.enabled)
    );
    assert_eq!(
        written.repository.cleanup.merged.enabled,
        Some(default_effective.cleanup.merged.enabled)
    );
    assert_eq!(
        written.repository.cleanup.merged.idle_minutes,
        Some(default_effective.cleanup.merged.idle_minutes)
    );
    assert_eq!(
        written.repository.cleanup.restore.retention_days,
        Some(default_effective.cleanup.restore.retention_days)
    );
    assert_eq!(
        written.repository.cleanup.restore.archive_max_mb,
        Some(default_effective.cleanup.restore.archive_max_mb)
    );
    assert_eq!(
        (written.repository.ports.start, written.repository.ports.end),
        (
            Some(default_effective.ports.start),
            Some(default_effective.ports.end)
        )
    );
    assert_eq!(
        written_effective.cleanup.auto.delay(),
        default_effective.cleanup.auto.delay()
    );
    assert_eq!(
        (
            written.machine.simulators.max_booted,
            written.machine.simulators.max_devices,
            written.machine.simulators.idle_seconds,
            written.machine.simulators.allow_any
        ),
        (
            defaults.machine.simulators.max_booted,
            defaults.machine.simulators.max_devices,
            defaults.machine.simulators.idle_seconds,
            defaults.machine.simulators.allow_any
        )
    );
    assert!(
        written.machine.simulators.default.is_none()
            && written.machine.simulators.profiles.is_empty()
    );
    assert!(
        written.repository.resources.is_empty() && written.repository.resource_pools.is_empty()
    );
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
    assert_eq!(
        config.repository.agent.default,
        "codex".parse::<crate::agent::Agent>().ok()
    );
    assert!(config.machine.simulators.profiles.contains_key("phone"));
    assert!(config.repository.resource_pools.contains_key("devices"));
}

#[test]
fn install_writes_the_template_once_and_keeps_edits() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(home.path());
    let expected = home.path().join(".config/shoal/config.toml");
    let (path, created) = Config::install_at(expected.clone()).unwrap();
    assert!(created && path == expected);
    assert_eq!(fs::read_to_string(&path).unwrap(), TEMPLATE);
    fs::write(&path, "agent.default = 'claude'\n").unwrap();
    let (same, created) = Config::install_at(expected.clone()).unwrap();
    assert!(!created && same == expected);
    let text = fs::read_to_string(&path).unwrap();
    assert_eq!(
        Config::parse(&text, &paths)
            .unwrap()
            .repository
            .agent
            .default,
        "claude".parse::<crate::agent::Agent>().ok()
    );
    // Reset keeps the edited file as a backup and restores the template.
    let (same, backup) = crate::config::file::replace(expected.clone(), default_template()).unwrap();
    let backup = backup.unwrap();
    assert!(same == expected && backup == home.path().join(".config/shoal/config.toml.backup"));
    assert_eq!(fs::read_to_string(&backup).unwrap(), text);
    assert_eq!(fs::read_to_string(&path).unwrap(), TEMPLATE);
    fs::remove_file(&path).unwrap();
    assert_eq!(
        crate::config::file::replace(expected.clone(), default_template())
            .unwrap()
            .1,
        None
    );
    assert_eq!(fs::read_to_string(&backup).unwrap(), text);
}

#[test]
fn default_agent_accepts_agent_spellings_only() {
    assert_eq!(
        Config::from_toml("").unwrap().repository.agent.default,
        None
    );
    for (text, agent) in [
        (
            "agent.default = 'codex'",
            "codex".parse::<crate::agent::Agent>().unwrap(),
        ),
        (
            "agent.default = 'claude'",
            "claude".parse::<crate::agent::Agent>().unwrap(),
        ),
        (
            "agent.default = 'pi'",
            "pi".parse::<crate::agent::Agent>().unwrap(),
        ),
        (
            "agent.default = 'happy-codex'",
            "happy-codex".parse::<crate::agent::Agent>().unwrap(),
        ),
    ] {
        let config = Config::from_toml(text).unwrap();
        assert_eq!(config.repository.agent.default, Some(agent));
    }
    let error = Config::from_toml("agent.default = 'two words'")
        .unwrap_err()
        .to_string();
    assert!(error.contains("lowercase"), "{error}");
}

#[test]
fn agent_limits_accept_dynamic_fixed_and_unlimited_values() {
    for (value, expected) in [
        ("'dynamic'", AgentLimit::Dynamic),
        ("'none'", AgentLimit::Unlimited),
        ("'unlimited'", AgentLimit::Unlimited),
        (
            "3",
            AgentLimit::Fixed(std::num::NonZeroU32::new(3).unwrap()),
        ),
    ] {
        let text = format!("agent.max_running = {value}\n[machine]\nmax_agents = {value}");
        let config = Config::from_toml(&text).unwrap();
        assert_eq!(config.machine.max_agents, expected);
        assert_eq!(config.repository.agent.max_running, Some(expected));
    }
    for value in ["0", "'automatic'"] {
        assert!(Config::from_toml(&format!("agent.max_running = {value}")).is_err());
        assert!(Config::from_toml(&format!("[machine]\nmax_agents = {value}")).is_err());
    }
    assert_eq!(
        Config::default()
            .effective(&RepoConfig::default())
            .unwrap()
            .agent
            .max_running,
        AgentLimit::Dynamic
    );
}

#[test]
fn load_sets_overload_defaults_on_the_machine_only() {
    let paths = Paths::for_test("/home/test");
    let config = Config::parse(
        "[machine]\nload = 'light'\n[machine.overload.cpu]\nused_percent = 85",
        &paths,
    )
    .unwrap();
    assert_eq!(config.machine.overload.warning.cpu_used_percent, 60);
    assert_eq!(config.machine.overload.recovery.cpu_used_percent, 55);
    assert_eq!(config.machine.overload.cpu.used_percent, 85);
    assert!(Config::parse("[machine]\nload = 'heavy'", &paths).is_err());
    assert!(Config::parse("load = 'light'", &paths).is_err());
    assert!(crate::config::repo::parse("load = 'full'").is_err());
}

#[test]
fn keep_awake_is_an_opt_in_machine_setting() {
    let paths = Paths::for_test("/home/test");
    assert!(!Config::parse("", &paths).unwrap().machine.keep_awake);
    assert!(
        Config::parse("[machine]\nkeep_awake = true", &paths)
            .unwrap()
            .machine
            .keep_awake
    );
    assert!(crate::config::repo::parse("keep_awake = true").is_err());
}

#[test]
fn cleanup_defaults_can_be_disabled_and_typos_are_rejected() {
    let config = Config::from_toml("").unwrap();
    assert_eq!(config.repository.cleanup.auto.enabled, None);
    let effective = config.effective(&RepoConfig::default()).unwrap();
    assert!(effective.cleanup.auto.enabled);
    assert!(effective.cleanup.pr.enabled);
    assert_eq!(effective.cleanup.auto.idle_minutes, 10);
    assert_eq!(
        Config::from_toml("[cleanup.pr]\nenabled=false")
            .unwrap()
            .repository
            .cleanup
            .pr
            .enabled,
        Some(false)
    );
    assert!(Config::from_toml("[cleanup.pr]\nenabld=false").is_err());
    let config = Config::from_toml("[cleanup.auto]\nenabled = false\nidle_minutes = 30\n").unwrap();
    assert_eq!(config.repository.cleanup.auto.enabled, Some(false));
    assert_eq!(config.repository.cleanup.auto.idle_minutes, Some(30));
    assert!(Config::from_toml("[auto_cleanpu]\nenabled = false").is_err());
}

#[test]
fn repository_values_win_over_global_cleanup_policy() {
    let global = Config::from_toml("[cleanup.auto]\nenabled = false\nidle_minutes = 30\n").unwrap();
    let repo =
        repo::parse("[cleanup.auto]\nenabled = true\n[cleanup.pr]\nenabled = false\n").unwrap();
    let effective = global.effective(&repo).unwrap();
    assert_eq!(
        effective.cleanup.auto.delay(),
        Some(Duration::from_secs(1800))
    );
    assert!(!effective.cleanup.pr.enabled);
    let repo = repo::parse("[cleanup.auto]\nidle_minutes = 5\n").unwrap();
    let effective = global.effective(&repo).unwrap();
    assert_eq!(effective.cleanup.auto.delay(), None);
    assert_eq!(effective.cleanup.auto.idle_minutes, 5);
    assert!(effective.cleanup.pr.enabled);
    assert_eq!(
        Config::default()
            .effective(&RepoConfig::default())
            .unwrap()
            .cleanup
            .auto
            .delay(),
        Some(Duration::from_secs(600))
    );
}

#[test]
fn agent_defaults_come_from_the_repository_before_the_global_config() {
    use crate::agent::AgentMode;

    let global =
        Config::from_toml("agent.default = 'claude'\n[agents.codex]\ndefault_mode = 'app'\n")
            .unwrap();
    let repo = repo::parse("agent.default = 'codex'\n").unwrap();
    let effective = global.effective(&repo).unwrap();
    assert_eq!(
        effective.agent.default,
        "codex".parse::<crate::agent::Agent>().ok()
    );
    assert_eq!(effective.agents["codex"].default_mode, AgentMode::App);
    let repo = repo::parse("[agents.codex]\ndefault_mode = 'cli'\n").unwrap();
    let effective = global.effective(&repo).unwrap();
    assert_eq!(
        effective.agent.default,
        "claude".parse::<crate::agent::Agent>().ok()
    );
    assert_eq!(effective.agents["codex"].default_mode, AgentMode::Cli);
    let effective = Config::default().effective(&repo).unwrap();
    assert_eq!(effective.agent.default, None);
}

#[test]
fn launch_profiles_run_with_their_base_tool_as_a_repository_sets_it() {
    let global = Config::from_toml(
        "[agents.claude-direct]\nbase = 'claude'\nenv_remove = ['ANTHROPIC_BASE_URL']\n",
    )
    .unwrap();
    let repo = repo::parse("[agents.claude]\ncommand = ['claude', '--verbose']\n").unwrap();
    let effective = global.effective(&repo).unwrap();
    assert!(crate::ai::agents(&effective.tools).any(|agent| agent == "claude-direct"));
    assert_eq!(
        effective.integration("claude-direct"),
        crate::ai::IntegrationKind::Claude
    );
    assert_eq!(
        effective.integration("codex"),
        crate::ai::IntegrationKind::Codex
    );
    assert_eq!(
        effective.integration("undefined"),
        crate::ai::IntegrationKind::None
    );
    assert_eq!(
        effective.launchers["claude-direct"],
        ["claude", "--verbose"]
    );
    let direct = effective.launch("claude-direct");
    assert_eq!(direct.arguments, effective.launch("claude").arguments);
    assert!(direct.environment.remove.contains("ANTHROPIC_BASE_URL"));
    assert_eq!(effective.launch("claude").environment, Default::default());
    assert_eq!(effective.launch("undefined"), &Default::default());
}

fn change(key: &str, value: Option<&str>) -> edit::Change {
    edit::Change {
        key: key.into(),
        value: value.map(Into::into),
    }
}

fn edit_one(paths: &Paths, key: &str, value: Option<&str>) -> Result<()> {
    Config::edit(paths, &[change(key, value)]).map(drop)
}

#[test]
fn global_edits_must_leave_a_valid_range_with_the_defaults() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(home.path());
    let error = edit_one(&paths, "ports.end", Some("4000"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("invalid edited config"), "{error}");
    assert!(edit_one(&paths, "ports.end", Some("0")).is_err());
    edit_one(&paths, "ports.start", Some("3000")).unwrap();
    edit_one(&paths, "ports.end", Some("4000")).unwrap();
    let ports = Config::load(&paths)
        .unwrap()
        .resolve(&ConfigLayers::default())
        .unwrap()
        .into_settings()
        .ports;
    assert_eq!((ports.start, ports.end), (3000, 4000));
    // Removing the start leaves the default start above the saved end.
    assert!(edit_one(&paths, "ports.start", None).is_err());
    edit_one(&paths, "ports.end", None).unwrap();
    edit_one(&paths, "ports.start", None).unwrap();
    assert!(Config::parse("[ports]\nstart = 60000\n", &paths).is_ok());
}

#[test]
fn dependent_global_edits_validate_together() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(home.path());
    let profile = [
        change("machine.simulators.profiles.phone.runtime", Some("iOS 27")),
        change("machine.simulators.default", Some("phone")),
        change(
            "machine.simulators.profiles.phone.device",
            Some("iPhone 17"),
        ),
    ];
    for single in &profile[..2] {
        assert!(Config::edit(&paths, std::slice::from_ref(single)).is_err());
    }
    assert!(!Config::path(&paths).exists());
    Config::edit(&paths, &profile).unwrap();
    let simulators = Config::load(&paths).unwrap().machine.simulators;
    assert_eq!(simulators.default.as_deref(), Some("phone"));
    assert_eq!(simulators.profiles["phone"].runtime, "iOS 27");
    // Removing a profile its default still names fails unless both go at once.
    assert!(edit_one(&paths, "machine.simulators.profiles.phone", None).is_err());
    Config::edit(
        &paths,
        &[
            change("machine.simulators.default", None),
            change("machine.simulators.profiles.phone", None),
        ],
    )
    .unwrap();
    assert!(
        Config::load(&paths)
            .unwrap()
            .machine
            .simulators
            .profiles
            .is_empty()
    );
}

#[test]
fn port_range_layers_per_bound_and_must_stay_nonempty() {
    let global = Config::from_toml("[ports]\nstart = 3000\nend = 3100\n").unwrap();
    let repo = repo::parse("[ports]\nstart = 3050\n[ports.web]\nport = 8080\n").unwrap();
    let ports = global.effective(&repo).unwrap().into_settings().ports;
    assert_eq!((ports.start, ports.end), (3050, 3100));
    let ports = global
        .effective(&RepoConfig::default())
        .unwrap()
        .into_settings()
        .ports;
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
        let config = Config::from_toml(&format!("[machine]\nroot_dir = {value:?}")).unwrap();
        assert_eq!(config.root_dir(&paths).unwrap(), PathBuf::from(expected));
    }
    for value in ["", "relative/repos", "~someone/repos"] {
        let config = Config::from_toml(&format!("[machine]\nroot_dir = {value:?}")).unwrap();
        assert!(config.root_dir(&paths).is_err());
    }
    assert!(Config::from_toml("repositories_dir = \"~/old\"").is_err());
}

#[test]
fn done_cleanup_defaults_on_and_can_be_overridden_per_repository() {
    let enabled =
        |global: &Config, repo: &RepoConfig| global.effective(repo).unwrap().cleanup.done.enabled;
    let global = Config::from_toml("[cleanup.done]\nenabled = false\n").unwrap();
    assert!(enabled(&Config::default(), &RepoConfig::default()));
    assert!(!enabled(&global, &RepoConfig::default()));
    let repo = repo::parse("[cleanup.done]\nenabled = true\n").unwrap();
    assert!(enabled(&global, &repo));
    assert!(Config::from_toml("[cleanup.done]\nenabled = 'yes'").is_err());
    assert!(Config::from_toml("[done]\ncleanup = false").is_err());
}

#[test]
fn environment_entries_validate_in_global_and_repository_config() {
    let paths = Paths::for_test("/home/test");
    for text in [
        "[env]\nSESSION = '{workspace}'\nempty = ''\n_1 = '$HOME $(false)'",
        "[env]\nPATH = '/custom/bin'",
    ] {
        assert!(Config::parse(text, &paths).is_ok(), "{text}");
        assert!(repo::parse(text).is_ok(), "{text}");
    }
    for text in [
        "[env]\nSHOAL_SCOPE_TOKEN = 'bad'",
        "[env]\nSHOAL_PORT_WEB = 'bad'",
        "[env]\n'bad=name' = 'bad'",
        "[env]\n'' = 'bad'",
        "[env]\n'1BAD' = 'bad'",
        "[env]\n'BAD-NAME' = 'bad'",
        "[env]\nBAD = 1",
        "[env]\nBAD = false",
        r#"[env]
BAD = "a\u0000b""#,
    ] {
        assert!(Config::parse(text, &paths).is_err(), "{text}");
        assert!(repo::parse(text).is_err(), "{text}");
    }
}

#[test]
fn agent_directories_are_absolute_or_under_home() {
    for valid in ["agent.dirs = ['/tmp', '~/cache']", "agent.dirs = []"] {
        repo::parse(valid).unwrap();
    }
    for invalid in ["relative", "~someone/cache", ""] {
        let error = repo::parse(&format!("agent.dirs = [{invalid:?}]")).unwrap_err();
        assert!(format!("{error:#}").contains("agent.dirs"), "{error:#}");
    }
}

#[test]
fn an_empty_repository_env_remove_keeps_every_variable() {
    let global =
        Config::from_toml("[agents.claude]\nenv_remove = ['ANTHROPIC_API_KEY']\n").unwrap();
    let removed = |repo: &str| {
        global
            .effective(&repo::parse(repo).unwrap())
            .unwrap()
            .launch("claude")
            .environment
            .remove
            .contains("ANTHROPIC_API_KEY")
    };
    assert!(removed(""));
    assert!(!removed("[agents.claude]\nenv_remove = []\n"));
}
