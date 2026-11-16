use std::fs;

use super::*;
use crate::agents::ensure_available;

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
        with_proxy_args(&settings, &name.into(), &mut command);
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
    with_proxy_args(&plain, &"claude".into(), &mut command);
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
    assert!(ensure_available(&"pi".into(), &settings).is_err());
    // Every launch, a resumed one included, prepares its proxy this way.
    assert!(launch_proxy(&settings, &"pi".into()).is_err());
    let claude = launch_proxy(&settings, &"claude".into()).unwrap().unwrap();
    assert_eq!(claude.upstream.as_deref(), Some("http://127.0.0.1:8317"));
    assert!(launch_proxy(&settings, &"codex".into()).unwrap().is_none());
    // Claude can; whether it is installed is another matter.
    if let Err(error) = ensure_available(&"claude".into(), &settings) {
        assert!(!error.to_string().contains("proxy"), "{error}");
    }
}

/// Where an agent with integration `kind` launched as `args` in `home`
/// sends its requests without a proxy, with only `variables` set.
fn default_upstream(
    kind: IntegrationKind,
    args: &[&str],
    home: &Path,
    variables: &[(&str, &str)],
) -> String {
    try_default_upstream(kind, args, home, variables).unwrap()
}

/// [`default_upstream`], or why Shoal cannot find it.
fn try_default_upstream(
    kind: IntegrationKind,
    args: &[&str],
    home: &Path,
    variables: &[(&str, &str)],
) -> Result<String> {
    let variable = |name: &str| {
        let set = variables.iter().find(|(set, _)| *set == name);
        set.map(|(_, value)| OsString::from(value))
    };
    let args: Vec<OsString> = args.iter().map(Into::into).collect();
    integration(kind).default_upstream(&args, home, &variable, home)
}

#[test]
fn claude_goes_to_the_base_url_its_settings_or_environment_name_else_anthropics() {
    let (project, user) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let url = |args: &[&str], variables: &[(&str, &str)]| {
        default_upstream(IntegrationKind::Claude, args, project.path(), variables)
    };
    assert_eq!(url(&["claude"], &[]), "https://api.anthropic.com");
    let user_dir = user.path().to_str().unwrap();
    let set = [
        ("ANTHROPIC_BASE_URL", "http://environment"),
        ("CLAUDE_CONFIG_DIR", user_dir),
    ];
    assert_eq!(url(&["claude"], &set), "http://environment");
    // Each settings file wins over the environment and those before it: the
    // user's, the project's shared, its local, then the launch's own.
    let base_url = |url: &str| format!(r#"{{"env":{{"ANTHROPIC_BASE_URL":"{url}"}}}}"#);
    fs::write(user.path().join("settings.json"), base_url("http://user")).unwrap();
    assert_eq!(url(&["claude"], &set), "http://user");
    let claude = project.path().join(".claude");
    fs::create_dir(&claude).unwrap();
    fs::write(claude.join("settings.json"), base_url("http://shared")).unwrap();
    assert_eq!(url(&["claude"], &set), "http://shared");
    fs::write(claude.join("settings.local.json"), base_url("http://local")).unwrap();
    assert_eq!(url(&["claude"], &set), "http://local");
    let given = base_url("http://given");
    assert_eq!(url(&["claude", "--settings", &given], &set), "http://given");
    // Only the sources the launch enables count, the last choice winning.
    fn only<'a>(sources: &[&'a str]) -> Vec<&'a str> {
        [&["claude"], sources].concat()
    }
    assert_eq!(
        url(&only(&["--setting-sources", "user"]), &set),
        "http://user"
    );
    assert_eq!(
        url(&only(&["--setting-sources=project,user"]), &set),
        "http://shared"
    );
    let twice = ["--setting-sources", "user", "--setting-sources", "local"];
    assert_eq!(url(&only(&twice), &set), "http://local");
    assert_eq!(
        url(&only(&["--setting-sources", ""]), &set),
        "http://environment"
    );
}

#[test]
fn codex_goes_to_its_providers_base_url_else_the_one_its_login_uses() {
    let home = tempfile::tempdir().unwrap();
    let codex_home = home.path().join(".codex");
    fs::create_dir(&codex_home).unwrap();
    let url = |args: &[&str], variables: &[(&str, &str)]| {
        default_upstream(IntegrationKind::Codex, args, home.path(), variables)
    };
    let login = |auth: &str| fs::write(codex_home.join("auth.json"), auth).unwrap();
    let chatgpt = "https://chatgpt.com/backend-api/codex";
    let api = "https://api.openai.com/v1";
    assert_eq!(url(&["codex"], &[]), chatgpt);
    login(r#"{"auth_mode":"chatgpt","OPENAI_API_KEY":null,"tokens":{}}"#);
    assert_eq!(url(&["codex"], &[]), chatgpt);
    login(r#"{"auth_mode":"apikey","OPENAI_API_KEY":"sk-test"}"#);
    assert_eq!(url(&["codex"], &[]), api);
    // A file from before Codex recorded the mode.
    login(r#"{"OPENAI_API_KEY":"sk-test"}"#);
    assert_eq!(url(&["codex"], &[]), api);
    login(r#"{"OPENAI_API_KEY":null,"tokens":{}}"#);
    assert_eq!(url(&["codex"], &[]), chatgpt);
    let moved = [("OPENAI_BASE_URL", "http://moved/v1")];
    assert_eq!(url(&["codex"], &moved), "http://moved/v1");
    // A provider of the user's goes to its own base URL, else the login's.
    fs::write(
        codex_home.join("config.toml"),
        "[model_providers.gateway]\nname = 'Gateway'\nbase_url = 'http://gateway/v1'\n\
         [model_providers.bare]\nname = 'Bare'\n",
    )
    .unwrap();
    let gateway = ["codex", "-c", "model_provider=gateway"];
    assert_eq!(url(&gateway, &moved), "http://gateway/v1");
    assert_eq!(url(&["codex", "-c", "model_provider=bare"], &[]), chatgpt);
    // A login Codex may keep in the keyring is one Shoal cannot read, so only
    // a base URL the launch names says where its requests go.
    let keyring = ["codex", "-c", "cli_auth_credentials_store=keyring"];
    let refused = |args: &[&str]| {
        try_default_upstream(IntegrationKind::Codex, args, home.path(), &[])
            .unwrap_err()
            .to_string()
    };
    // The error names the setting that would say, for the provider in use.
    let error = refused(&keyring);
    assert!(error.contains("OPENAI_BASE_URL"), "{error}");
    let bare_keyring = [&keyring[..], &["-c", "model_provider=bare"]].concat();
    let error = refused(&bare_keyring);
    assert!(error.contains("model_providers.bare.base_url"), "{error}");
    assert_eq!(url(&keyring, &moved), "http://moved/v1");
    let gateway_keyring = [&keyring[..], &gateway[1..]].concat();
    assert_eq!(url(&gateway_keyring, &[]), "http://gateway/v1");
}
