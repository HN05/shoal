use super::*;

#[test]
fn settings_deliver_messages_report_state_and_wake_the_idle_agent() {
    let args = Claude.hook_args(false).unwrap();
    assert_eq!(args[0], "--settings");
    let settings: Value = serde_json::from_str(args[1].to_str().unwrap()).unwrap();
    for event in hooks::MESSAGE_EVENTS {
        let command = settings["hooks"][event][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(command.ends_with(" messages --hook"), "{command}");
    }
    let states: Vec<_> = state_hooks("shoal")
        .into_iter()
        .filter(|(event, _)| *event != "StopFailure")
        .map(|(event, group)| {
            let command = group["hooks"][0]["command"].as_str().unwrap().to_owned();
            let state = command.split(' ').nth(3).unwrap().to_owned();
            (event, group["matcher"].as_str().map(str::to_owned), state)
        })
        .collect();
    let expected = |event, matcher: Option<&str>, state: &str| {
        (event, matcher.map(str::to_owned), state.to_owned())
    };
    assert_eq!(
        states,
        [
            expected("SessionStart", Some("startup|resume|clear"), "idle"),
            expected("UserPromptSubmit", None, "working"),
            expected("PostToolUse", None, "working"),
            expected("PostToolUseFailure", None, "working"),
            expected(
                "Notification",
                Some("permission_prompt|elicitation_dialog"),
                "waiting"
            ),
            expected("Notification", Some("idle_prompt"), "idle"),
            expected("Stop", None, "idle"),
        ]
    );
    // A turn an API error ended reports its failure, read from the event.
    let failed = settings["hooks"]["StopFailure"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(
        failed.contains(" internal turn-failed --integration claude "),
        "{failed}"
    );
    // A second group waits in the background to wake the idle agent,
    // after a failed turn as after one that ended normally.
    for event in ["Stop", "StopFailure"] {
        let wake = &settings["hooks"][event][1]["hooks"][0];
        assert!(
            wake["command"]
                .as_str()
                .unwrap()
                .ends_with(" internal wake --agent \"$PPID\""),
            "{event}: {wake}"
        );
        assert_eq!(wake["asyncRewake"], true);
        assert_eq!(wake["timeout"], WAKE_TIMEOUT_SECS);
    }
    // Worktrees Claude Code creates are child workspaces, and each
    // subagent learns which workspace it works in.
    for (event, command) in [
        ("WorktreeCreate", " internal worktree-create"),
        ("WorktreeRemove", " internal worktree-remove"),
        ("SubagentStart", " internal subagent-start"),
    ] {
        let handler = &settings["hooks"][event][0]["hooks"][0];
        assert!(
            handler["command"].as_str().unwrap().ends_with(command),
            "{handler}"
        );
    }
    // The agent may finish its own assignment without a permission check.
    let allow = settings["permissions"]["allow"].as_array().unwrap();
    for rule in [
        "Bash(shoal done)",
        "Bash(shoal --json done)",
        "Bash(shoal --json done *)",
        "Bash(shoal --json pr *)",
    ] {
        assert!(allow.contains(&json!(rule)), "{rule}: {allow:?}");
    }
}

#[test]
fn stop_failure_errors_name_the_turn_failure() {
    for (error, failure) in [
        ("rate_limit", Some(TurnFailure::RateLimited)),
        ("overloaded", Some(TurnFailure::Overloaded)),
        ("server_error", Some(TurnFailure::ServerError)),
        ("authentication_failed", Some(TurnFailure::Other)),
    ] {
        let event = json!({"hook_event_name": "StopFailure", "error": error});
        assert_eq!(Claude.turn_failure(&event), failure, "{error}");
    }
    assert_eq!(
        Claude.turn_failure(&json!({"hook_event_name": "Stop"})),
        None
    );
}

#[test]
fn chat_settings_deliver_messages_and_wake_without_turn_state() {
    let args = Claude.chat_hook_args().unwrap();
    let settings: Value = serde_json::from_str(args[1].to_str().unwrap()).unwrap();
    assert!(!settings.to_string().contains("agent-state"), "{settings}");
    // A chat belongs to no workspace and finishes no assignment.
    assert!(settings.get("permissions").is_none(), "{settings}");
    assert!(
        settings["hooks"].get("WorktreeCreate").is_none(),
        "{settings}"
    );
    for event in ["Stop", "StopFailure"] {
        let wake = &settings["hooks"][event][0]["hooks"][0]["command"];
        assert!(wake.as_str().unwrap().contains(" internal wake "), "{wake}");
    }
    for event in hooks::MESSAGE_EVENTS {
        assert!(settings["hooks"][event][0]["hooks"][0]["command"].is_string());
    }
}

#[test]
fn proxied_settings_send_api_requests_to_the_proxy() {
    let env = |proxied| {
        let args = Claude.hook_args(proxied).unwrap();
        let settings: Value = serde_json::from_str(args[1].to_str().unwrap()).unwrap();
        settings["env"].clone()
    };
    assert_eq!(env(false), Value::Null);
    assert_eq!(env(true), json!({"ANTHROPIC_BASE_URL": crate::proxy::URL}));
    let mut args = Claude.hook_args(true).unwrap();
    let home = tempfile::tempdir().unwrap();
    let environment = AgentEnvironment {
        home: home.path(),
        configured: &Default::default(),
    };
    let routing = super::super::Routing {
        url: "http://127.0.0.1:4000",
        directory: home.path(),
        private: home.path(),
    };
    Claude
        .route_through_proxy(&mut args, &routing, &environment)
        .unwrap();
    let settings: Value =
        serde_json::from_str(&std::fs::read_to_string(&args[1]).unwrap()).unwrap();
    assert_eq!(
        settings["env"]["ANTHROPIC_BASE_URL"],
        "http://127.0.0.1:4000"
    );
}

#[test]
fn the_proxys_key_joins_the_agents_own_headers() {
    let none = |_: &str| None;
    let mine = |name: &str| (name == CUSTOM_HEADERS).then(|| "x-team: blue".into());
    let header = format!("{}: sk", crate::proxy::KEY_HEADER);
    assert_eq!(
        Claude.proxy_environment("sk", &none),
        [(CUSTOM_HEADERS, OsString::from(&header))]
    );
    assert_eq!(
        Claude.proxy_environment("sk", &mine),
        [(
            CUSTOM_HEADERS,
            OsString::from(format!("x-team: blue\n{header}"))
        )]
    );
}
