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
fn sensitive_paths_are_relative_to_the_repository_root() {
    let conflicts = parse("[conflicts]\nsensitive = ['src/version.rs', 'migrations/**']\n")
        .unwrap()
        .conflicts;
    assert_eq!(
        conflicts.sensitive.unwrap(),
        ["src/version.rs", "migrations/**"]
    );
    for pattern in [
        "",
        "/version",
        ":(exclude)version",
        "../version",
        "a/../../b",
    ] {
        let text = format!("[conflicts]\nsensitive = ['{pattern}']\n");
        assert!(parse(&text).is_err(), "{text}");
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
        assert!(parse(text).is_err(), "{text}");
    }
}

#[test]
fn no_agent_names_the_reserved_tool_all() {
    for text in [
        "[agents.all]\ncommand = ['all']\n",
        "[agents.helper]\nbase = 'all'\n",
    ] {
        assert!(parse(text).is_err(), "{text}");
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
    for &kind in crate::config::hooks::HookKind::ALL {
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
            .0
            .setup
            .command
            .is_none()
    );
    let directory = repository.path().join(LOCAL_DIR);
    fs::create_dir(&directory).unwrap();
    // A rule beside it works without a config file.
    fs::create_dir_all(directory.join("rules")).unwrap();
    fs::write(directory.join("rules/chat.md"), "Local chat").unwrap();
    let (config, files) = load_local(repository.path()).unwrap();
    assert_eq!(config.rules["chat"].text.as_deref(), Some("Local chat"));
    assert_eq!(
        files,
        modes::Files::from([("rules.chat.text".into(), directory.join("rules/chat.md"))])
    );
    fs::write(
        local_config_path(repository.path()),
        "[setup]\ncommand = 'setup.sh'\n[hooks]\npost_setup = '/abs/attach'\n\
         [secrets.TOKEN]\ncommand = ['token']\n[setup.seed]\ncommand = 'scripts/seed'\n",
    )
    .unwrap();
    fs::create_dir_all(directory.join("modes")).unwrap();
    fs::write(directory.join("modes/worker.md"), "Local additions").unwrap();
    let (config, _) = load_local(repository.path()).unwrap();
    let expected = directory.join("setup.sh");
    assert_eq!(config.setup.command.as_deref(), expected.to_str());
    assert_eq!(config.hooks.post_setup.as_deref(), Some("/abs/attach"));
    assert!(config.secrets.contains_key("TOKEN"));
    // The seed runs in the worktree, so its path stays relative to it.
    assert_eq!(config.setup.seed.command.as_deref(), Some("scripts/seed"));
    assert_eq!(
        config.modes["worker"].instructions.as_deref(),
        Some("Local additions")
    );
}

#[test]
fn worktree_rules_and_modes_come_from_the_root_or_the_shoal_directory() {
    let read = |files: &[(&str, &[u8])], toml: &str| {
        let worktree = tempfile::tempdir().unwrap();
        for directory in [".shoal/rules", ".shoal/modes", "rules", "modes"] {
            fs::create_dir_all(worktree.path().join(directory)).unwrap();
        }
        for (path, text) in files {
            fs::write(worktree.path().join(path), text).unwrap();
        }
        fs::write(worktree.path().join(".shoal.toml"), toml).unwrap();
        load(worktree.path()).map(|(config, files)| {
            let files: Vec<_> = files
                .into_iter()
                .map(|(key, path)| {
                    let path = path.strip_prefix(worktree.path()).unwrap().to_owned();
                    (key, path)
                })
                .collect();
            (config, files)
        })
    };
    for directory in ["", ".shoal/"] {
        let rule = format!("{directory}rules/worker.md");
        let mode = format!("{directory}modes/review-docs.md");
        let (config, files) = read(&[(&rule, b"Worker"), (&mode, b"Docs")], "").unwrap();
        assert_eq!(config.rules["worker"].text.as_deref(), Some("Worker"));
        // A file names a mode no table declares.
        let docs = &config.modes["review-docs"];
        assert_eq!(docs.instructions.as_deref(), Some("Docs"));
        assert_eq!(docs.kind, None);
        assert_eq!(
            files,
            [
                ("modes.review-docs.instructions".into(), mode.into()),
                ("rules.worker.text".into(), rule.into()),
            ]
        );
    }
    // Each file stands alone, and inline TOML wins over its file, which is
    // never read.
    let (config, files) = read(
        &[
            ("rules/worker.md", b"\xff not UTF-8"),
            (".shoal/rules/chat.md", b"Chat"),
            ("rules/notes.txt", b"Not a rule"),
        ],
        "rules.worker.text = 'Inline'",
    )
    .unwrap();
    assert_eq!(config.rules["worker"].text.as_deref(), Some("Inline"));
    assert_eq!(config.rules["chat"].text.as_deref(), Some("Chat"));
    assert!(!config.rules.contains_key("notes"));
    assert_eq!(
        files,
        [("rules.chat.text".into(), ".shoal/rules/chat.md".into())]
    );
    let error = read(
        &[
            ("rules/chat.md", b"Root"),
            (".shoal/rules/chat.md", b"Shoal"),
        ],
        "",
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains(".shoal/rules/chat.md"), "{error}");
    let error = read(&[("modes/.hidden.md", b"Hidden")], "")
        .unwrap_err()
        .to_string();
    assert!(error.ends_with("modes/.hidden.md"), "{error}");
}

#[test]
fn cache_names_are_lowercase_identifiers() {
    let configured = |names: &[&str]| {
        names
            .iter()
            .map(|name| {
                let cache = Cache {
                    scope: CacheScope::Shared,
                };
                ((*name).to_owned(), cache)
            })
            .collect::<BTreeMap<_, _>>()
    };
    validate_caches(&configured(&["go", "pnpm_store", "gradle-home"])).unwrap();
    for name in ["", "Go", ".go", "../go", "go/build"] {
        assert!(validate_caches(&configured(&[name])).is_err(), "{name:?}");
    }
}

#[test]
fn the_default_mode_starts_only_in_a_workspace() {
    let error = parse("[modes.worker]\nkind = 'chat'\n")
        .unwrap_err()
        .to_string();
    assert!(
        error.starts_with("modes.worker.kind must be workspace"),
        "{error}"
    );
    parse("[modes.worker]\nkind = 'workspace'\n[modes.talk]\nkind = 'chat'\n").unwrap();
}
