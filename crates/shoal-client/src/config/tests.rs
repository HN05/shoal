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
fn global_git_profile_selections_must_name_defined_profiles() {
    let paths = Paths::for_test("/home/test");
    let profiles = "[machine.git_profiles.agent]\nuser.email = 'agent@example.invalid'\n";
    for key in ["git_profile", "agent.git_profile"] {
        assert!(Config::parse(&format!("{key} = 'agent'\n{profiles}"), &paths).is_ok());
        assert!(Config::parse(&format!("{key} = 'missing'\n{profiles}"), &paths).is_err());
    }
    assert!(crate::config::repo::parse("[agent]\ngit_profile = 'bad name'").is_err());
}

#[test]
fn additional_hook_paths_are_validated_in_global_config() {
    let paths = Paths::for_test("/home/test");
    for &kind in crate::config::hooks::HookKind::ALL {
        let key = kind.key();
        if kind == crate::config::hooks::HookKind::Setup {
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
fn a_home_other_than_the_users_keeps_its_config_inside() {
    let home = tempfile::tempdir().unwrap();
    assert_eq!(
        Config::path_for_home(home.path()),
        home.path().join(".config/shoal/config.toml")
    );
}

#[test]
fn agent_and_job_limits_accept_dynamic_fixed_and_unlimited_values() {
    for (value, expected) in [
        ("'dynamic'", RunLimit::Dynamic),
        ("'none'", RunLimit::Unlimited),
        ("'unlimited'", RunLimit::Unlimited),
        ("3", RunLimit::Fixed(std::num::NonZeroU32::new(3).unwrap())),
    ] {
        let text = format!(
            "agent.max_running = {value}\n[machine]\nmax_agents = {value}\nmax_jobs = {value}"
        );
        let config = Config::from_toml(&text).unwrap();
        assert_eq!(config.machine.max_agents, expected);
        assert_eq!(config.machine.max_jobs, expected);
        assert_eq!(config.repository.agent.max_running, Some(expected));
    }
    for value in ["0", "'automatic'"] {
        assert!(Config::from_toml(&format!("agent.max_running = {value}")).is_err());
        assert!(Config::from_toml(&format!("[machine]\nmax_agents = {value}")).is_err());
    }
}

#[test]
fn load_sets_overload_defaults_on_the_machine_only() {
    let paths = Paths::for_test("/home/test");
    let config = Config::parse(
        "[machine]\nload = 'light'\n[machine.overload.cpu]\nused_percent = 85",
        &paths,
    )
    .unwrap();
    // A light load lowers what the file leaves out and keeps what it sets.
    let balanced = overload::Overload::default();
    let overload = &config.machine.overload;
    assert!(overload.warning.cpu_used_percent < balanced.warning.cpu_used_percent);
    assert!(overload.recovery.cpu_used_percent < balanced.recovery.cpu_used_percent);
    assert_eq!(overload.cpu.used_percent, 85);
    assert!(Config::parse("[machine]\nload = 'heavy'", &paths).is_err());
    assert!(Config::parse("load = 'light'", &paths).is_err());
    assert!(crate::config::repo::parse("load = 'full'").is_err());
}

#[test]
fn keep_awake_is_a_machine_setting() {
    let paths = Paths::for_test("/home/test");
    assert!(
        Config::parse("[machine]\nkeep_awake = true", &paths)
            .unwrap()
            .machine
            .keep_awake
    );
    assert!(crate::config::repo::parse("keep_awake = true").is_err());
}

#[test]
fn cleanup_policies_can_be_disabled_and_typos_are_rejected() {
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
}

#[test]
fn agent_defaults_come_from_the_repository_before_the_global_config() {
    let global = Config::from_toml("agent.default = 'claude'\n").unwrap();
    let repo = repo::parse("agent.default = 'codex'\n").unwrap();
    let effective = global.effective(&repo).unwrap();
    assert_eq!(
        effective.agent.default,
        "codex".parse::<crate::agent::Agent>().ok()
    );
    let repo = repo::parse("").unwrap();
    let effective = global.effective(&repo).unwrap();
    assert_eq!(
        effective.agent.default,
        "claude".parse::<crate::agent::Agent>().ok()
    );
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
        effective.launchers["claude-direct"],
        ["claude", "--verbose"]
    );
    let direct = effective.launch("claude-direct");
    assert_eq!(direct.arguments, effective.launch("claude").arguments);
    assert!(direct.environment.remove.contains("ANTHROPIC_BASE_URL"));
    assert_eq!(effective.launch("claude").environment, Default::default());
    assert_eq!(effective.launch("undefined"), &Default::default());
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
fn done_cleanup_can_be_overridden_per_repository() {
    let enabled =
        |global: &Config, repo: &RepoConfig| global.effective(repo).unwrap().cleanup.done.enabled;
    let global = Config::from_toml("[cleanup.done]\nenabled = false\n").unwrap();
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

#[test]
fn a_launch_model_comes_from_the_request_then_the_choosing_setting_then_the_tool() {
    use crate::agent::ModelChoice;
    let global = Config::from_toml(
        "[agent]\ndefault = 'codex'\neffort = 'xhigh'\n[agents.codex]\nmodel = 'gpt'\neffort = 'low'\n[agents.claude]\neffort = 'medium'\n",
    )
    .unwrap();
    let effective = global.effective(&RepoConfig::default()).unwrap();
    let choice = |model: &str, effort: &str| ModelChoice {
        model: Some(model.to_owned()),
        effort: Some(effort.to_owned()),
    };
    // `[agent]` effort applies to the agent `default` chooses only.
    assert_eq!(
        effective.model("codex", Default::default()),
        choice("gpt", "xhigh")
    );
    assert_eq!(
        effective.model("claude", Default::default()),
        ModelChoice {
            model: None,
            effort: Some("medium".into()),
        }
    );
    let requested = ModelChoice {
        model: Some("o9".into()),
        effort: None,
    };
    assert_eq!(effective.model("codex", requested), choice("o9", "xhigh"));
    assert!(effective.model("pi", Default::default()).is_empty());
    let paths = Paths::for_test("/home/test");
    assert!(Config::parse("[agent]\nmodel = ''\n", &paths).is_err());
}

#[test]
fn a_global_mode_may_name_a_rule_only_its_file_defines() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(home.path());
    let file = Config::path(&paths);
    let rules = file.parent().unwrap().join("rules");
    std::fs::create_dir_all(&rules).unwrap();
    std::fs::write(&file, "[modes.triage]\nrules = ['triage']\n").unwrap();
    std::fs::write(rules.join("triage.md"), "Label it.").unwrap();
    // The file's text stays out of the config `config set` writes back.
    let written = Config::load(&paths).unwrap();
    assert!(!written.repository.rules.contains_key("triage"));
    // What resolves it without the files, such as the agents' tools, works.
    written.tools().unwrap();
    let settings = Config::load_with_templates(&paths)
        .unwrap()
        .resolve(&Default::default())
        .unwrap();
    assert_eq!(settings.rules["triage"].text, "Label it.");
    std::fs::remove_file(rules.join("triage.md")).unwrap();
    assert!(Config::load(&paths).is_err());
}

#[test]
fn a_snapshot_resolves_with_the_template_files_beside_its_file() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(home.path());
    let file = Config::path(&paths);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "[review]\nagent = 'codex'\n").unwrap();
    let snapshot = Config::load(&paths).unwrap();
    // Edits after the snapshot reach only the template files.
    std::fs::write(&file, "[review]\nagent = 'claude'\n").unwrap();
    let rules = file.parent().unwrap().join("rules");
    std::fs::create_dir(&rules).unwrap();
    std::fs::write(rules.join("review.md"), "Follow the brief.").unwrap();
    let settings = snapshot
        .resolve_with_template_files(&Default::default(), &paths)
        .unwrap();
    assert_eq!(settings.rules["review"].text, "Follow the brief.");
    assert_eq!(
        settings.review.agent.as_ref().map(|agent| agent.name()),
        Some("codex")
    );
    let plain = snapshot.resolve(&Default::default()).unwrap();
    assert_eq!(plain.rules["review"].text, templates::review_base());
}

#[test]
fn the_runner_takes_its_settings_and_refuses_labels_it_cannot_register() {
    let config =
        Config::from_toml("[runner]\nenabled = true\nlabels = ['rust']\npermit = 'builds'")
            .unwrap();
    let settings = Config::default().effective(&config.repository).unwrap();
    assert!(settings.runner.enabled);
    assert_eq!(settings.runner.labels, ["rust"]);
    assert_eq!(settings.runner.permit.as_deref(), Some("builds"));
    for label in ["''", "'two words'", "'a,b'", "'shoal:host'"] {
        let config = Config::from_toml(&format!("runner.labels = [{label}]")).unwrap();
        let error = config.repository.validate().unwrap_err().to_string();
        assert!(error.contains("runner.labels"), "{error}");
    }
}

#[test]
fn no_config_layer_or_file_defines_the_setup_mode() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(home.path());
    for config in ["[modes.setup]\nrules = ['work']\n", "[modes.setup]\n"] {
        assert!(Config::parse(config, &paths).is_err(), "{config}");
        assert!(crate::config::repo::parse(config).is_err(), "{config}");
    }
    let file = Config::path(&paths);
    let modes = file.parent().unwrap().join("modes");
    std::fs::create_dir_all(&modes).unwrap();
    std::fs::write(&file, "").unwrap();
    std::fs::write(modes.join("setup.md"), "Write anything.").unwrap();
    let error = Config::load(&paths).unwrap_err();
    assert!(format!("{error:#}").contains("setup.md"), "{error:#}");
}

#[test]
fn only_the_global_config_starts_a_mode_without_a_repository() {
    let paths = Paths::for_test("/home/test");
    let mode = "[modes.manage]\nkind = 'chat'\nrepository = false\n";
    assert!(Config::parse(mode, &paths).is_ok());
    let error = crate::config::repo::parse(mode).unwrap_err().to_string();
    assert!(error.contains("modes.manage.repository"), "{error}");
    assert!(crate::config::repo::parse("[modes.manage]\nkind = 'chat'\n").is_ok());
}
