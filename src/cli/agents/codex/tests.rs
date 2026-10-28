use std::collections::BTreeMap;

use super::*;

#[test]
fn config_overrides_deliver_messages_and_report_state() {
    let hooks: BTreeMap<_, _> = Codex
        .hook_args(false)
        .unwrap()
        .chunks(2)
        .map(|flag| {
            assert_eq!(flag[0], "-c");
            let (key, value) = flag[1].to_str().unwrap().split_once('=').unwrap();
            let parsed: toml::Table = toml::from_str(&format!("value = {value}")).unwrap();
            (key.to_owned(), parsed["value"].clone())
        })
        .collect();
    assert_eq!(
        hooks.keys().collect::<Vec<_>>(),
        [
            "hooks.PostToolUse",
            "hooks.SessionStart",
            "hooks.Stop",
            "hooks.SubagentStart",
            "hooks.UserPromptSubmit"
        ]
    );
    for event in hooks::MESSAGE_EVENTS {
        let groups = &hooks[&format!("hooks.{event}")];
        let handler = &groups[0]["hooks"][0];
        assert_eq!(handler["type"].as_str(), Some("command"));
        assert!(
            handler["command"]
                .as_str()
                .unwrap()
                .ends_with(" messages --hook")
        );
        let state = groups[1]["hooks"][0]["command"].as_str().unwrap();
        assert!(state.contains(" agent-state working "), "{state}");
    }
    let stop = hooks["hooks.Stop"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(stop.contains(" agent-state idle "), "{stop}");
    let start = &hooks["hooks.SessionStart"][0];
    assert_eq!(start["matcher"].as_str(), Some(hooks::SESSION_START));
    let start = start["hooks"][0]["command"].as_str().unwrap();
    assert!(start.contains(" agent-state idle "), "{start}");
    let chat = Codex.chat_hook_args().unwrap();
    assert!(
        chat.iter()
            .all(|arg| !arg.to_str().unwrap().contains("agent-state"))
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
        let mut args = Codex.hook_args(true).unwrap();
        assert_eq!(args.last().unwrap(), PROVIDER);
        let routing = args.len() - 1;
        let configured = BTreeMap::from([("CODEX_HOME".to_owned(), codex.clone().into())]);
        let environment = AgentEnvironment {
            home: home.path(),
            configured: &configured,
        };
        Codex
            .route_through_proxy(
                &mut args,
                "http://127.0.0.1:4000",
                &environment,
                home.path(),
            )
            .map(|()| args.split_off(routing))
    };
    // A provider of the user's keeps its token and only moves.
    let custom = routed(Some("model_provider = 'cliproxy'\n")).unwrap();
    assert_eq!(
        custom,
        [
            "-c",
            "model_providers.cliproxy.base_url=\"http://127.0.0.1:4000\"",
            "-c",
            "model_providers.cliproxy.supports_websockets=false",
            "-c",
            "model_providers.cliproxy.env_http_headers={ x-shoal-proxy-key = \"SHOAL_PROXY_KEY\" }",
        ]
    );
    // The built-in one gives way to Shoal's provider with its login.
    for config in [None, Some("model_provider = 'openai'\n")] {
        let builtin = routed(config).unwrap();
        assert_eq!(builtin[1], "model_provider=\"shoal-proxy\"");
        let provider = builtin[3].to_str().unwrap();
        assert!(provider.contains("requires_openai_auth=true"), "{provider}");
        assert!(
            provider.contains(r#"env_http_headers={ x-shoal-proxy-key = "SHOAL_PROXY_KEY" }"#),
            "{provider}"
        );
        assert!(
            provider.contains(r#"base_url="http://127.0.0.1:4000""#),
            "{provider}"
        );
    }
    let error = format!(
        "{:#}",
        routed(Some("model_provider = 'oss'\n")).unwrap_err()
    );
    assert!(error.contains("built-in provider \"oss\""), "{error}");
    // Without a proxy the hooks name no provider.
    assert!(
        !Codex
            .hook_args(false)
            .unwrap()
            .iter()
            .any(|arg| arg == PROVIDER)
    );
}
