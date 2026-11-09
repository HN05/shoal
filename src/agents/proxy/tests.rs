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
    assert_eq!(claude.upstream, "http://127.0.0.1:8317");
    assert!(launch_proxy(&settings, &"codex".into()).unwrap().is_none());
    // Claude can; whether it is installed is another matter.
    if let Err(error) = ensure_available(&"claude".into(), &settings) {
        assert!(!error.to_string().contains("proxy"), "{error}");
    }
}
