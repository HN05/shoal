use super::*;
use crate::agent::AgentMode;

#[test]
fn setup_cmd_requires_a_nonempty_path() {
    assert!(parse("").unwrap().setup.command.is_none());
    assert_eq!(
        parse("setup.command = 'scripts/setup.sh'")
            .unwrap()
            .setup
            .command
            .as_deref(),
        Some("scripts/setup.sh")
    );
    for text in [
        "setup.command = ''",
        "setup.command = '  '",
        "setup.command = []",
        r#"setup.command = "a\u0000b""#,
        "hooks.post_setup = ''",
        "hooks.pre_remove = ' '",
    ] {
        assert!(parse(text).is_err(), "{text}");
    }
}

#[test]
fn layering_keeps_omitted_options_and_replaces_named_entries() {
    let base = parse(
        "agent.default = 'claude'\nsetup.command = 'base/setup'\nhooks.post_setup = 'base/attach'\n\
             [ports]\non_conflict = 'auto'\nstart = 3000\nend = 3100\n\
             [ports.web]\nport = 3000\n[ports.api]\nport = 4000\n\
             [resources.lock]\ncapacity = 1\n[simulators]\npreferred = ['phone']\n\
             [cleanup.auto]\nenabled = false\nidle_minutes = 30\n",
    )
    .unwrap();
    let local = parse(
        "setup.command = 'local/setup'\n[ports]\nend = 3050\n[ports.web]\nenv = 'LOCAL_PORT'\n\
             [resources.signing]\ncapacity = 2\n[cleanup.auto]\nenabled = true\n[agents.codex]\ndefault_mode = 'app'\n",
    )
    .unwrap();
    let config = local.over(base);
    assert_eq!(
        config.agent.default,
        "claude".parse::<crate::agent::Agent>().ok()
    );
    assert_eq!(config.agents["codex"].default_mode, Some(AgentMode::App));
    assert_eq!(config.setup.command.as_deref(), Some("local/setup"));
    assert_eq!(config.hooks.post_setup.as_deref(), Some("base/attach"));
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
    assert!(parse("agent.default = 'Happy'\n").is_err());
    assert!(parse("[agents.codex]\ndefault_mode = 'desktop'\n").is_err());
}

#[test]
fn seed_paths_stay_inside_the_worktree() {
    let seed = parse("[setup.seed]\ncommand = 'scripts/seed'\npaths = ['target', 'ios/build']\n")
        .unwrap()
        .setup
        .seed;
    assert_eq!(seed.command.as_deref(), Some("scripts/seed"));
    assert_eq!(seed.paths, ["target", "ios/build"]);
    for text in [
        "[setup.seed]\ncommand = ' '",
        "[setup.seed]\ncommand = '/usr/bin/make'",
        "[setup.seed]\ncommand = '../seed.sh'",
        "[setup.seed]\npaths = ['']",
        "[setup.seed]\npaths = ['/target']",
        "[setup.seed]\npaths = ['../target']",
        "[setup.seed]\npaths = ['./target']",
        "[setup.seed]\npaths = ['.git']",
        "[setup.seed]\npaths = ['target', 'target']",
        "[setup.seed]\npaths = ['target', 'target/debug']",
        "[setup.seed]\npaths = ['ios/build', 'ios']",
        "[setup.seed]\npaths = ['build/../..']",
        "[setup.seed]\ncmd = 'x'",
    ] {
        assert!(parse(text).is_err(), "{text}");
    }
}

#[test]
fn former_option_names_are_unknown_keys() {
    for text in [
        "[auto_cleanup]\nenabled = false",
        "[pr_cleanup]\nenabled = false",
        "agent_template = 'x'",
        "issue_template = 'x'",
        "[done]\nautomatic = true",
    ] {
        let error = parse(text).unwrap_err().to_string();
        assert!(error.contains("unknown field"), "{text}: {error}");
    }
}

#[test]
fn no_agent_names_the_reserved_tool_all() {
    for text in [
        "[agents.all]\ncommand = ['all']\n",
        "[agents.helper]\nbase = 'all'\n",
    ] {
        let error = format!("{:#}", parse(text).unwrap_err());
        assert!(
            error.contains("reserved for skill installation"),
            "{text}: {error}"
        );
    }
}

#[test]
fn config_serializes_in_its_own_spelling() {
    let config = parse(
        "agent.default = 'happy-codex'\n[ports.web]\nport = 1\n[agents.codex]\ndefault_mode = 'app'\n",
    )
    .unwrap();
    let json = serde_json::to_value(&config).unwrap();
    assert_eq!(json["agent"]["default"], "happy-codex");
    assert_eq!(json["agents"]["codex"]["default_mode"], "app");
    let back: RepoConfig = serde_json::from_value(json).unwrap();
    assert_eq!(
        back.agent.default,
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
        let pointer = format!("/{}", key.replace('.', "/"));
        assert_eq!(
            serde_json::to_value(inherited)
                .unwrap()
                .pointer(&pointer)
                .unwrap(),
            "/base/hook"
        );
        let config = parse(&format!("{key} = 'scripts/hook'"))
            .unwrap()
            .over(base);
        assert_eq!(
            serde_json::to_value(config)
                .unwrap()
                .pointer(&pointer)
                .unwrap(),
            "scripts/hook"
        );
    }
}

#[test]
fn the_local_config_names_scripts_beside_it_and_may_hold_secrets() {
    let repository = tempfile::tempdir().unwrap();
    assert!(
        load_local(repository.path())
            .unwrap()
            .setup
            .command
            .is_none()
    );
    let directory = repository.path().join(LOCAL_DIR);
    fs::create_dir(&directory).unwrap();
    // A template beside it works without a config file.
    fs::write(directory.join("chat-template.md"), "Local chat").unwrap();
    let templates = load_local(repository.path()).unwrap().agent.templates;
    assert_eq!(templates.chat.as_deref(), Some("Local chat"));
    fs::write(
        local_config_path(repository.path()),
        "[setup]\ncommand = 'setup.sh'\n[hooks]\npost_setup = '/abs/attach'\n\
         [secrets.TOKEN]\ncommand = ['token']\n[setup.seed]\ncommand = 'scripts/seed'\n",
    )
    .unwrap();
    fs::write(directory.join("worker-template.md"), "Local additions").unwrap();
    let config = load_local(repository.path()).unwrap();
    let expected = directory.join("setup.sh");
    assert_eq!(config.setup.command.as_deref(), expected.to_str());
    assert_eq!(config.hooks.post_setup.as_deref(), Some("/abs/attach"));
    assert!(config.secrets.contains_key("TOKEN"));
    // The seed runs in the worktree, so its path stays relative to it.
    assert_eq!(config.setup.seed.command.as_deref(), Some("scripts/seed"));
    assert_eq!(
        config.agent.templates.worker.as_deref(),
        Some("Local additions")
    );
}

#[test]
fn worktree_templates_come_from_the_root_or_the_shoal_directory() {
    let read = |files: &[(&str, &str)], toml: &str| {
        let worktree = tempfile::tempdir().unwrap();
        fs::create_dir(worktree.path().join(".shoal")).unwrap();
        for (path, text) in files {
            fs::write(worktree.path().join(path), text).unwrap();
        }
        fs::write(worktree.path().join(".shoal.toml"), toml).unwrap();
        load(worktree.path()).map(|config| config.agent.templates)
    };
    for directory in ["", ".shoal/"] {
        let templates = read(
            &[
                (&format!("{directory}worker-template.md"), "Worker"),
                (&format!("{directory}chat-template.md"), "Chat"),
            ],
            "",
        )
        .unwrap();
        assert_eq!(templates.worker.as_deref(), Some("Worker"));
        assert_eq!(templates.chat.as_deref(), Some("Chat"));
    }
    // Each template stands alone, and inline TOML wins over its file.
    let templates = read(
        &[
            ("worker-template.md", "Worker"),
            (".shoal/chat-template.md", "Chat"),
        ],
        "agent.templates.worker = 'Inline'",
    )
    .unwrap();
    assert_eq!(templates.worker.as_deref(), Some("Inline"));
    assert_eq!(templates.chat.as_deref(), Some("Chat"));
    for name in ["worker-template.md", "chat-template.md"] {
        let error = read(&[(name, "Root"), (&format!(".shoal/{name}"), "Shoal")], "")
            .unwrap_err()
            .to_string();
        assert!(error.starts_with("both "), "{error}");
        assert!(
            error.ends_with(&format!("{name} exist; keep only one {name}")),
            "{error}"
        );
        assert!(error.contains(&format!(".shoal/{name}")), "{error}");
    }
}
