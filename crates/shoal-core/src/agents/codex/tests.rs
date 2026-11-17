use std::collections::BTreeMap;

use super::*;
use crate::agents::tests::WORKSPACE;

#[test]
fn a_chat_agent_runs_a_workspace_agents_hooks_without_the_subagent_brief() {
    let mut chat = Codex.hook_args(AgentSpace::Chat, false).unwrap();
    let mut workspace = Codex.hook_args(WORKSPACE.into(), false).unwrap();
    let starts = |args: &[OsString]| {
        (args.iter())
            .position(|arg| arg.to_str().unwrap().starts_with("hooks.SubagentStart="))
            .unwrap()
    };
    let (in_chat, in_workspace) = (starts(&chat), starts(&workspace));
    // Every agent reports its subagents' starts; a workspace's also briefs
    // them.
    let (chat_starts, workspace_starts) = (
        chat.remove(in_chat).into_string().unwrap(),
        workspace.remove(in_workspace).into_string().unwrap(),
    );
    assert!(chat_starts.contains("internal subagent --integration codex"));
    assert!(!chat_starts.contains("subagent-start"));
    assert!(workspace_starts.contains("subagent-start"));
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

#[test]
fn mcp_servers_set_every_field_as_a_config_override() {
    let server = crate::config::mcp::Server {
        command: "xcrun".into(),
        args: vec!["mcpbridge".into()],
        env: [("LOG".to_owned(), "1 \"quoted\"".to_owned())].into(),
        enabled: true,
    };
    let args = Codex.mcp_args(&[("xcode", &server)]);
    let overrides: Vec<&str> = args.iter().map(|arg| arg.to_str().unwrap()).collect();
    let (flags, values): (Vec<_>, Vec<_>) =
        overrides.chunks(2).map(|pair| (pair[0], pair[1])).unzip();
    assert!(flags.iter().all(|flag| *flag == "-c"));
    // Each value parses as the TOML line Codex reads it as.
    let config: toml::Table = toml::from_str(&values.join("\n")).unwrap();
    assert_eq!(
        config["mcp_servers"]["xcode"],
        toml::toml! {
            command = "xcrun"
            args = ["mcpbridge"]
            env = { LOG = "1 \"quoted\"" }
        }
        .into()
    );
}
