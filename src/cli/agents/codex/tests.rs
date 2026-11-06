use std::collections::BTreeMap;

use super::*;
use crate::cli::agents::tests::WORKSPACE;

#[test]
fn a_chat_agent_runs_a_workspace_agents_hooks_without_the_subagent_brief() {
    let chat = Codex.hook_args(AgentSpace::Chat, false).unwrap();
    let mut workspace = Codex.hook_args(WORKSPACE.into(), false).unwrap();
    let brief = (workspace.iter())
        .position(|arg| arg.to_str().unwrap().starts_with("hooks.SubagentStart="))
        .unwrap();
    // Its `-c` goes with it.
    workspace.drain(brief - 1..=brief);
    assert_eq!(chat, workspace);
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
        let mut args = Codex.hook_args(WORKSPACE.into(), true).unwrap();
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
            .hook_args(WORKSPACE.into(), false)
            .unwrap()
            .iter()
            .any(|arg| arg == PROVIDER)
    );
}
