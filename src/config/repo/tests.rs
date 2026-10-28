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
    assert_eq!(
        config.default_agent,
        "claude".parse::<crate::agent::Agent>().ok()
    );
    assert_eq!(config.codex.default_mode, Some(AgentMode::App));
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
    assert!(parse("default_agent = 'Happy'\n").is_err());
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
        Some("happy-codex".parse::<crate::agent::Agent>().unwrap())
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
