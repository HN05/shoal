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
fn codex_mode_defaults_to_cli_and_rejects_invalid_settings() {
    use crate::agent::CodexMode;

    for text in ["", "[codex]"] {
        let config: Config = toml::from_str(text).unwrap();
        assert_eq!(config.codex.default_mode, None);
        let effective = config.effective(&RepoConfig::default()).unwrap();
        assert_eq!(effective.codex.default_mode, CodexMode::Cli);
    }
    let config: Config = toml::from_str("[codex]\ndefault_mode = 'app'").unwrap();
    assert_eq!(config.codex.default_mode, Some(CodexMode::App));
    for text in [
        "[codex]\ndefault_mode = 'desktop'",
        "[codex]\ndefaut_mode = 'app'",
    ] {
        assert!(toml::from_str::<Config>(text).is_err());
    }
}

#[test]
fn additional_hook_paths_are_validated_in_global_config() {
    let paths = Paths::for_test("/home/test");
    for &kind in crate::hooks::HookKind::ALL {
        let key = kind.key();
        if !kind.allows_global() {
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
    assert_eq!(written.default_agent, None);
    // The template writes the built-in defaults out explicitly.
    let written_effective = written.effective(&RepoConfig::default()).unwrap();
    let default_effective = defaults.effective(&RepoConfig::default()).unwrap();
    assert_eq!(
        written.codex.default_mode,
        Some(default_effective.codex.default_mode)
    );
    assert_eq!(
        written.auto_cleanup.enabled,
        Some(default_effective.auto_cleanup.enabled)
    );
    assert_eq!(
        written.auto_cleanup.idle_minutes,
        Some(default_effective.auto_cleanup.idle_minutes)
    );
    assert_eq!(
        written.pr_cleanup.enabled,
        Some(default_effective.pr_cleanup.enabled)
    );
    assert_eq!(
        (written.ports.start, written.ports.end),
        (
            Some(default_effective.ports.start),
            Some(default_effective.ports.end)
        )
    );
    assert_eq!(
        written_effective.auto_cleanup.delay(),
        default_effective.auto_cleanup.delay()
    );
    assert_eq!(
        (
            written.simulators.max_booted,
            written.simulators.max_devices,
            written.simulators.idle_seconds,
            written.simulators.allow_any
        ),
        (
            defaults.simulators.max_booted,
            defaults.simulators.max_devices,
            defaults.simulators.idle_seconds,
            defaults.simulators.allow_any
        )
    );
    assert!(written.simulators.default.is_none() && written.simulators.profiles.is_empty());
    assert!(written.resources.is_empty() && written.resource_pools.is_empty());
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
    assert_eq!(config.default_agent, Some(crate::agent::Agent::Codex));
    assert!(config.simulators.profiles.contains_key("phone"));
    assert!(config.resource_pools.contains_key("devices"));
}

#[test]
fn install_writes_the_template_once_and_keeps_edits() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(home.path());
    let expected = home.path().join(".config/shoal/config.toml");
    let (path, created) = Config::install_at(expected.clone()).unwrap();
    assert!(created && path == expected);
    assert_eq!(fs::read_to_string(&path).unwrap(), TEMPLATE);
    fs::write(&path, "default_agent = 'claude'\n").unwrap();
    let (same, created) = Config::install_at(expected.clone()).unwrap();
    assert!(!created && same == expected);
    let text = fs::read_to_string(&path).unwrap();
    assert_eq!(
        Config::parse(&text, &paths).unwrap().default_agent,
        Some(crate::agent::Agent::Claude)
    );
    // Reset keeps the edited file as a backup and restores the template.
    let (same, backup) = Config::replace_at(expected.clone(), default_template()).unwrap();
    let backup = backup.unwrap();
    assert!(same == expected && backup == home.path().join(".config/shoal/config.toml.backup"));
    assert_eq!(fs::read_to_string(&backup).unwrap(), text);
    assert_eq!(fs::read_to_string(&path).unwrap(), TEMPLATE);
    fs::remove_file(&path).unwrap();
    assert_eq!(
        Config::replace_at(expected.clone(), default_template())
            .unwrap()
            .1,
        None
    );
    assert_eq!(fs::read_to_string(&backup).unwrap(), text);
}

#[test]
fn default_agent_accepts_agent_spellings_only() {
    use crate::agent::{Agent, BuiltinAgent};

    assert_eq!(toml::from_str::<Config>("").unwrap().default_agent, None);
    for (text, agent) in [
        ("default_agent = 'codex'", Agent::Codex),
        ("default_agent = 'claude'", Agent::Claude),
        ("default_agent = 'pi'", Agent::Custom("pi".into())),
        (
            "default_agent = 'happy-codex'",
            Agent::Happy(BuiltinAgent::Codex),
        ),
    ] {
        let config: Config = toml::from_str(text).unwrap();
        assert_eq!(config.default_agent, Some(agent));
    }
    let error = toml::from_str::<Config>("default_agent = 'happy'")
        .unwrap_err()
        .to_string();
    assert!(error.contains("happy-claude"), "{error}");
}

#[test]
fn cleanup_defaults_can_be_disabled_and_typos_are_rejected() {
    let config: Config = toml::from_str("").unwrap();
    assert_eq!(config.auto_cleanup.enabled, None);
    let effective = config.effective(&RepoConfig::default()).unwrap();
    assert!(effective.auto_cleanup.enabled);
    assert!(effective.pr_cleanup.enabled);
    assert_eq!(effective.auto_cleanup.idle_minutes, 10);
    assert_eq!(
        toml::from_str::<Config>("[pr_cleanup]\nenabled=false")
            .unwrap()
            .pr_cleanup
            .enabled,
        Some(false)
    );
    assert!(toml::from_str::<Config>("[pr_cleanup]\nenabld=false").is_err());
    let config: Config =
        toml::from_str("[auto_cleanup]\nenabled = false\nidle_minutes = 30\n").unwrap();
    assert_eq!(config.auto_cleanup.enabled, Some(false));
    assert_eq!(config.auto_cleanup.idle_minutes, Some(30));
    assert!(toml::from_str::<Config>("[auto_cleanpu]\nenabled = false").is_err());
}

#[test]
fn repository_values_win_over_global_cleanup_policy() {
    let global: Config =
        toml::from_str("[auto_cleanup]\nenabled = false\nidle_minutes = 30\n").unwrap();
    let repo =
        repo::parse("[auto_cleanup]\nenabled = true\n[pr_cleanup]\nenabled = false\n").unwrap();
    let effective = global.effective(&repo).unwrap();
    assert_eq!(
        effective.auto_cleanup.delay(),
        Some(Duration::from_secs(1800))
    );
    assert!(!effective.pr_cleanup.enabled);
    let repo = repo::parse("[auto_cleanup]\nidle_minutes = 5\n").unwrap();
    let effective = global.effective(&repo).unwrap();
    assert_eq!(effective.auto_cleanup.delay(), None);
    assert_eq!(effective.auto_cleanup.idle_minutes, 5);
    assert!(effective.pr_cleanup.enabled);
    assert_eq!(
        Config::default()
            .effective(&RepoConfig::default())
            .unwrap()
            .auto_cleanup
            .delay(),
        Some(Duration::from_secs(600))
    );
}

#[test]
fn agent_defaults_come_from_the_repository_before_the_global_config() {
    use crate::agent::{Agent, CodexMode};

    let global: Config =
        toml::from_str("default_agent = 'claude'\n[codex]\ndefault_mode = 'app'\n").unwrap();
    let repo = repo::parse("default_agent = 'codex'\n").unwrap();
    let effective = global.effective(&repo).unwrap();
    assert_eq!(effective.default_agent, Some(Agent::Codex));
    assert_eq!(effective.codex.default_mode, CodexMode::App);
    let repo = repo::parse("[codex]\ndefault_mode = 'cli'\n").unwrap();
    let effective = global.effective(&repo).unwrap();
    assert_eq!(effective.default_agent, Some(Agent::Claude));
    assert_eq!(effective.codex.default_mode, CodexMode::Cli);
    let effective = Config::default().effective(&repo).unwrap();
    assert_eq!(effective.default_agent, None);
}

#[test]
fn global_edits_must_leave_a_valid_range_with_the_defaults() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(home.path());
    let error = Config::edit(&paths, "ports.end", Some("4000"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("invalid edited config"), "{error}");
    assert!(Config::edit(&paths, "ports.end", Some("0")).is_err());
    Config::edit(&paths, "ports.start", Some("3000")).unwrap();
    Config::edit(&paths, "ports.end", Some("4000")).unwrap();
    let ports = Config::load(&paths)
        .unwrap()
        .resolve(&ConfigLayers::default())
        .unwrap()
        .ports;
    assert_eq!((ports.start, ports.end), (3000, 4000));
    // Removing the start leaves the default start above the saved end.
    assert!(Config::edit(&paths, "ports.start", None).is_err());
    Config::edit(&paths, "ports.end", None).unwrap();
    Config::edit(&paths, "ports.start", None).unwrap();
    assert!(Config::parse("[ports]\nstart = 60000\n", &paths).is_ok());
}

#[test]
fn port_range_layers_per_bound_and_must_stay_nonempty() {
    let global: Config = toml::from_str("[ports]\nstart = 3000\nend = 3100\n").unwrap();
    let repo = repo::parse("[ports]\nstart = 3050\n[ports.web]\nport = 8080\n").unwrap();
    let ports = global.effective(&repo).unwrap().ports;
    assert_eq!((ports.start, ports.end), (3050, 3100));
    let ports = global.effective(&RepoConfig::default()).unwrap().ports;
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
        let config: Config = toml::from_str(&format!("root_dir = {value:?}")).unwrap();
        assert_eq!(config.root_dir(&paths).unwrap(), PathBuf::from(expected));
    }
    for value in ["", "relative/repos", "~someone/repos"] {
        let config: Config = toml::from_str(&format!("root_dir = {value:?}")).unwrap();
        assert!(config.root_dir(&paths).is_err());
    }
    let config: Config = toml::from_str("repositories_dir = \"~/old\"").unwrap();
    assert_eq!(
        config.root_dir(&paths).unwrap(),
        PathBuf::from("/home/test/old")
    );
    let config: Config =
        toml::from_str("repositories_dir = \"~/old\"\nroot_dir = \"~/new\"").unwrap();
    assert_eq!(
        config.root_dir(&paths).unwrap(),
        PathBuf::from("/home/test/new")
    );
}
