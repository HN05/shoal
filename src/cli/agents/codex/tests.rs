use std::collections::BTreeMap;

use super::*;
use crate::cli::agents::tests::WORKSPACE;

#[test]
fn a_chat_agent_reports_only_its_session_start() {
    let chat = Codex.chat_hook_args().unwrap();
    let reports: Vec<_> = chat
        .iter()
        .map(|arg| arg.to_str().unwrap())
        .filter(|arg| arg.contains("agent-state"))
        .collect();
    assert!(
        matches!(reports.as_slice(), [start] if start.starts_with("hooks.SessionStart=")),
        "{reports:?}"
    );
    assert!(
        chat.iter()
            .any(|arg| arg.to_str().unwrap().contains("messages --hook"))
    );
}

#[test]
fn proxied_codex_sends_the_provider_its_config_uses_to_the_proxy() {
    let home = tempfile::tempdir().unwrap();
    let codex = home.path().join(".codex");
    std::fs::create_dir_all(&codex).unwrap();
    let routed = |config: Option<&str>| {
        match config {
            Some(config) => std::fs::write(codex.join("config.toml"), config).unwrap(),
            None => drop(std::fs::remove_file(codex.join("config.toml"))),
        }
        let mut args = Codex.hook_args(WORKSPACE, true).unwrap();
        assert_eq!(args.last().unwrap(), PROVIDER);
        let placeholder = args.len() - 1;
        let configured = BTreeMap::from([("CODEX_HOME".to_owned(), codex.clone().into())]);
        let environment = AgentEnvironment {
            home: home.path(),
            configured: &configured,
            inherited: |_| None,
        };
        let routing = super::super::Routing {
            url: "http://127.0.0.1:4000",
            directory: home.path(),
            private: home.path(),
        };
        Codex
            .route_through_proxy(&mut args, &routing, &environment)
            .map(|()| args.split_off(placeholder))
    };
    // A provider of the user's keeps its token and only moves.
    let custom = routed(Some("model_provider = 'cliproxy'\n")).unwrap();
    assert!(
        custom
            .iter()
            .any(|arg| arg == "model_providers.cliproxy.base_url=\"http://127.0.0.1:4000\""),
        "{custom:?}"
    );
    // The built-in one gives way to Shoal's provider with its login.
    for config in [None, Some("model_provider = 'openai'\n")] {
        let builtin = routed(config).unwrap();
        assert_eq!(builtin[1], "model_provider=\"shoal-proxy\"");
        let provider = builtin[3].to_str().unwrap();
        assert!(
            provider.contains(r#"base_url="http://127.0.0.1:4000""#),
            "{provider}"
        );
    }
    assert!(routed(Some("model_provider = 'oss'\n")).is_err());
    // Without a proxy the hooks name no provider.
    assert!(
        !Codex
            .hook_args(WORKSPACE, false)
            .unwrap()
            .iter()
            .any(|arg| arg == PROVIDER)
    );
}
