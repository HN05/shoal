use std::fs;

use super::*;
use crate::{config::named_commands, test_support::install_test_script};

#[test]
fn agents_are_installed_only_when_every_program_they_start_is_on_path() {
    let bin = tempfile::tempdir().unwrap();
    for program in ["claude", "probe"] {
        let path = bin.path().join(program);
        install_test_script(&path, "#!/bin/sh\n");
    }
    let mut commands = named_commands::defaults();
    for (name, program) in [
        ("probe", "probe".to_owned()),
        ("missing", "missing".to_owned()),
        ("absolute", bin.path().join("probe").display().to_string()),
        ("relative", "./scripts/agent".to_owned()),
    ] {
        commands.insert(name.into(), vec![program]);
    }
    let search_path = bin.path().as_os_str();
    let installed = |name: &str| installed(name, &commands, search_path);

    assert!(installed("claude"));
    assert!(!installed("codex"));
    // Happy needs both its own CLI and the agent it runs.
    #[cfg(feature = "happy")]
    {
        assert!(!installed("happy-claude"));
        install_test_script(&bin.path().join("happy"), "#!/bin/sh\n");
        assert!(installed("happy-claude"));
        assert!(!installed("happy-codex"));
    }

    assert!(installed("probe"));
    assert!(installed("absolute"));
    assert!(installed("relative"));
    assert!(!installed("missing"));
    assert!(!installed("undefined"));
}

#[test]
fn launches_name_the_missing_program_or_undefined_agent() {
    let mut commands = named_commands::defaults();
    commands.insert("probe".into(), vec!["shoal-test-missing-program".into()]);
    let error = ensure_installed("probe", &commands).unwrap_err();
    assert_eq!(
        error.to_string(),
        "cannot start probe: shoal-test-missing-program is not installed or not on PATH"
    );
    let error = ensure_installed("undefined", &commands).unwrap_err();
    assert!(error.to_string().starts_with("unknown agent"), "{error}");
}

#[test]
fn integrations_serve_only_the_names_they_list() {
    assert!(integration(IntegrationKind::Claude).trust_file().is_some());
    assert_eq!(integration(IntegrationKind::Codex).wakes(), Wake::Prompt);
    assert_eq!(integration(IntegrationKind::Claude).wakes(), Wake::Hook);
    assert_eq!(
        prompt_wake(IntegrationKind::Codex),
        Some(PromptWake::Always)
    );
    assert_eq!(
        prompt_wake(IntegrationKind::Claude),
        Some(PromptWake::AfterInterrupt)
    );
    let configured = integration(IntegrationKind::None);
    assert!(configured.trust_file().is_none());
    assert!(configured.hook_args(false).unwrap().is_empty());
    assert_eq!(
        builtin_names()[..2],
        ["codex".to_owned(), "claude".to_owned()]
    );
}

#[test]
fn a_configured_resume_command_still_goes_through_the_agents_proxy() {
    let config = crate::config::Config::from_toml(
        "[machine.proxies.local]\nupstream = 'http://127.0.0.1:8317'\n\
         \n[machine.agents.claude]\nproxy = 'local'\n[machine.agents.codex]\nproxy = 'local'",
    )
    .unwrap();
    let settings = config.resolve(&Default::default()).unwrap();
    let with = |name: &str, command: &[&str]| {
        let mut command: Vec<OsString> = command.iter().map(Into::into).collect();
        with_proxy_args(&settings, name, &mut command);
        command
    };
    // Claude gets settings of its own for the wrapper's merge to fill in.
    let claude = with("claude", &["claude", "--continue"]);
    assert_eq!(claude, ["claude", "--continue", "--settings", "{}"]);
    // Codex's provider placeholder comes before a prompt after `--`.
    assert_eq!(
        with("codex", &["saved-session", "--", "prompt"]),
        [
            "saved-session",
            "{shoal-proxy-codex-provider}",
            "--",
            "prompt"
        ]
    );
    // Then the wrapper routes them as it does a launch's own.
    let home = tempfile::tempdir().unwrap();
    let routing = Routing {
        url: "http://127.0.0.1:4000",
        directory: home.path(),
        private: home.path(),
    };
    let read = |path: &OsString| -> serde_json::Value {
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    };
    let routed = route_through_proxy(
        IntegrationKind::Claude,
        claude,
        &routing,
        home.path(),
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(
        read(&routed[3])["env"]["ANTHROPIC_BASE_URL"],
        "http://127.0.0.1:4000"
    );
    // A resume command's own settings stay, joined with the proxy's.
    let own = with(
        "claude",
        &["claude", "--settings", r#"{"model":"opus"}"#, "--continue"],
    );
    let routed = route_through_proxy(
        IntegrationKind::Claude,
        own,
        &routing,
        home.path(),
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(routed.iter().filter(|arg| *arg == "--settings").count(), 1);
    let joined = read(&routed[2]);
    assert_eq!(joined["model"], "opus", "{joined}");
    assert_eq!(joined["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:4000");
    // An agent without a proxy runs its command as configured.
    let plain = crate::config::Config::from_toml("").unwrap();
    let plain = plain.resolve(&Default::default()).unwrap();
    let mut command: Vec<OsString> = vec!["claude".into(), "--continue".into()];
    with_proxy_args(&plain, "claude", &mut command);
    assert_eq!(command, ["claude", "--continue"]);
}

#[test]
fn a_proxy_is_refused_for_an_agent_shoal_cannot_point_at_it() {
    let config = crate::config::Config::from_toml(
        "[machine.proxies.local]\nupstream = 'http://127.0.0.1:8317'\n\
         \n[machine.agents.pi]\nproxy = 'local'\n[machine.agents.claude]\nproxy = 'local'",
    )
    .unwrap();
    let settings = config.resolve(&Default::default()).unwrap();
    let error = ensure_available("pi", &settings).unwrap_err().to_string();
    assert!(
        error.contains("cannot send its API requests through a proxy"),
        "{error}"
    );
    // Every launch, a resumed one included, prepares its proxy this way.
    let error = launch_proxy(&settings, "pi").unwrap_err().to_string();
    assert!(
        error.contains("remove proxy from its [machine.agents.pi]"),
        "{error}"
    );
    let claude = launch_proxy(&settings, "claude").unwrap().unwrap();
    assert_eq!(claude.upstream, "http://127.0.0.1:8317");
    assert!(launch_proxy(&settings, "codex").unwrap().is_none());
    // Claude can; whether it is installed is another matter.
    if let Err(error) = ensure_available("claude", &settings) {
        assert!(!error.to_string().contains("proxy"), "{error}");
    }
}

#[test]
fn agents_get_each_configured_directory_as_they_resolve_it() {
    let settings = |text: &str| {
        let config = crate::config::Config::from_toml(text).unwrap();
        config
            .resolve(&crate::config::repo::ConfigLayers::default())
            .unwrap()
    };
    let home = tempfile::tempdir().unwrap();
    let defaults = settings("");
    let tmp = fs::canonicalize("/tmp").unwrap();
    assert_eq!(
        agent_dir_args(&defaults, "claude", home.path()),
        [OsString::from(format!("--add-dir={}", tmp.display()))]
    );
    assert_eq!(
        agent_dir_args(&defaults, "codex", home.path()),
        [OsString::from("--add-dir"), tmp.clone().into()]
    );
    // A tool that takes no other working directory gets none.
    assert!(agent_dir_args(&defaults, "pi", home.path()).is_empty());
    // `~/` names a path under the home directory; one reached again through
    // a symlink counts once.
    let configured = settings("agent.dirs = ['/tmp', '~/cache', '~/link']\n");
    let cache = home.path().join("cache");
    fs::create_dir(&cache).unwrap();
    std::os::unix::fs::symlink(&cache, home.path().join("link")).unwrap();
    assert_eq!(
        agent_dirs(&configured, home.path()),
        BTreeSet::from([tmp.clone(), fs::canonicalize(&cache).unwrap()])
    );
    assert!(agent_dirs(&settings("agent.dirs = []\n"), home.path()).is_empty());
    // A workspace's or chat's own directory follows the configured ones.
    assert_eq!(
        dir_args(
            &defaults,
            "codex",
            home.path(),
            Some(Path::new("/root/repo"))
        ),
        [
            OsString::from("--add-dir"),
            tmp.clone().into(),
            "--add-dir".into(),
            "/root/repo".into()
        ]
    );
    let chat = Chat {
        id: "chat-id".into(),
        repository_id: "repo".into(),
        name: "talk".into(),
        agent: "codex".into(),
        pid: 1,
        started_at: 0,
        origin: None,
        tmp_dir: Some(cache.join("chat-id")),
    };
    assert_eq!(
        chat_dir_args(&defaults, "codex", home.path(), &chat, &[]),
        [
            OsString::from("--add-dir"),
            tmp.into(),
            "--add-dir".into(),
            cache.join("chat-id").into()
        ]
    );
}

#[test]
fn only_agents_with_fork_arguments_start_from_a_fork() {
    let config = crate::config::Config::from_toml("[agents.droid]\ncommand = ['droid']\n").unwrap();
    let settings = config.resolve(&Default::default()).unwrap();
    let child = Path::new("/work/child");
    assert_eq!(
        fork_args(&settings, "claude", "s1", child).unwrap(),
        ["--resume", "s1", "--fork-session"]
    );
    // Codex works where the forked session was unless told the child's directory.
    assert_eq!(
        fork_args(&settings, "codex", "s1", child).unwrap(),
        ["fork", "s1", "--cd", "/work/child"]
    );
    let error = fork_args(&settings, "droid", "s1", child)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("cannot fork a session of droid: configure [agents.droid] fork"),
        "{error}"
    );
}

#[test]
fn a_launch_profile_opens_its_app_by_its_own_mode() {
    let config = crate::config::Config::from_toml(
        "[agents.codex-app]\nbase = 'codex'\ndefault_mode = 'app'\n\
         [agents.claude]\ndefault_mode = 'app'\n[agents.claude-cli]\nbase = 'claude'\n",
    )
    .unwrap();
    let settings = config.resolve(&Default::default()).unwrap();
    let opens_app = |name: &str| agent_options(&settings, name).app.is_some();
    assert!(opens_app("codex-app"));
    assert!(!opens_app("codex"));
    assert!(opens_app("claude"));
    // A profile that sets no mode of its own launches the CLI.
    assert!(!opens_app("claude-cli"));
}

#[test]
fn a_reviewer_needs_only_its_review_command_installed() {
    let config = crate::config::Config::from_toml(
        "[agents.present]\ncommand = ['sh']\nreview = ['{prompt}', '{schema}']\n\
         [agents.absent]\ncommand = ['no-such-reviewer']\nreview = ['{prompt}', '{schema}']\n",
    )
    .unwrap();
    let settings = config.resolve(&Default::default()).unwrap();
    ensure_reviewer_available("present", &settings).unwrap();
    let refused = ensure_reviewer_available("absent", &settings).unwrap_err();
    assert!(
        refused
            .to_string()
            .contains("no-such-reviewer is not installed"),
        "{refused}"
    );
    assert!(ensure_reviewer_available("unknown", &settings).is_err());
}
