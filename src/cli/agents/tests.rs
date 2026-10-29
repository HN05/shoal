use std::{fs, os::unix::fs::PermissionsExt};

use super::*;
use crate::config::named_commands;

#[test]
fn agents_are_installed_only_when_every_program_they_start_is_on_path() {
    let bin = tempfile::tempdir().unwrap();
    for program in ["claude", "probe"] {
        let path = bin.path().join(program);
        fs::write(&path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
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
        fs::copy(bin.path().join("claude"), bin.path().join("happy")).unwrap();
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
    let config: crate::config::Config = toml::from_str(
        "[proxy.local]\nupstream = 'http://127.0.0.1:8317'\n\
         [ai.claude]\nproxy = 'local'\n[ai.codex]\nproxy = 'local'\n",
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
    let plain: crate::config::Config = toml::from_str("").unwrap();
    let plain = plain.resolve(&Default::default()).unwrap();
    let mut command: Vec<OsString> = vec!["claude".into(), "--continue".into()];
    with_proxy_args(&plain, "claude", &mut command);
    assert_eq!(command, ["claude", "--continue"]);
}

#[test]
fn a_proxy_is_refused_for_an_agent_shoal_cannot_point_at_it() {
    let config: crate::config::Config = toml::from_str(
        "[proxy.local]\nupstream = 'http://127.0.0.1:8317'\n\
         [ai.pi]\nproxy = 'local'\n[ai.claude]\nproxy = 'local'\n",
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
    assert!(error.contains("remove proxy from its [ai.pi]"), "{error}");
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
        let config: crate::config::Config = toml::from_str(text).unwrap();
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
    let configured = settings("agent_dirs = ['/tmp', '~/cache', '~/link']\n");
    let cache = home.path().join("cache");
    fs::create_dir(&cache).unwrap();
    std::os::unix::fs::symlink(&cache, home.path().join("link")).unwrap();
    assert_eq!(
        agent_dirs(&configured, home.path()),
        BTreeSet::from([tmp, fs::canonicalize(&cache).unwrap()])
    );
    assert!(agent_dirs(&settings("agent_dirs = []\n"), home.path()).is_empty());
}
