use super::*;
use crate::cli::agents::tests::WORKSPACE;

/// The settings `args` pass, given as JSON.
fn settings_of(args: &[OsString]) -> Value {
    assert_eq!(args[0], "--settings");
    serde_json::from_str(args[1].to_str().unwrap()).unwrap()
}

#[test]
fn settings_deliver_messages_report_state_and_wake_the_idle_agent() {
    let settings = settings_of(&Claude.hook_args(WORKSPACE, false).unwrap());
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
    // A turn's end reports how long the agent's prompt cache lives.
    let ended = settings["hooks"]["Stop"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(
        ended.contains(" internal agent-state idle --ends-turn claude "),
        "{ended}"
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
    // The agent may finish its own assignment and stop what its commands
    // left running without a permission check.
    let allow = settings["permissions"]["allow"].as_array().unwrap();
    for rule in [
        "Bash(shoal done)",
        "Bash(shoal --json done)",
        "Bash(shoal --json done *)",
        "Bash(shoal --json pr *)",
        "Bash(shoal stop --leftovers)",
        "Bash(shoal --json stop --processes *)",
    ] {
        assert!(allow.contains(&json!(rule)), "{rule}: {allow:?}");
    }
    // Plain stop would stop the agent itself.
    for rule in ["Bash(shoal stop)", "Bash(shoal stop *)"] {
        assert!(!allow.contains(&json!(rule)), "{rule}: {allow:?}");
    }
}

#[test]
fn auto_mode_learns_that_the_workspace_branch_is_the_agents_own() {
    let settings = settings_of(&Claude.hook_args(WORKSPACE, false).unwrap());
    let environment = settings["autoMode"]["environment"].as_array().unwrap();
    // Claude Code keeps its own entries.
    assert_eq!(environment[0], "$defaults");
    assert_eq!(environment.len(), 2, "{environment:?}");
    let own = environment[1].as_str().unwrap();
    for named in ["`fix-login`", "`fix-login-redirect`"] {
        assert!(own.contains(named), "{named}: {own}");
    }
    // A workspace at a commit has no branch to call its own.
    let detached = AgentWorkspace {
        branch: None,
        ..WORKSPACE
    };
    let settings = settings_of(&Claude.hook_args(detached, false).unwrap());
    assert!(settings.get("autoMode").is_none(), "{settings}");
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
fn chat_settings_deliver_messages_report_the_session_and_wake_without_turn_state() {
    let settings = settings_of(&Claude.chat_hook_args().unwrap());
    // Only the session start reports, for the chat's session.
    let start = &settings["hooks"]["SessionStart"][0];
    assert_eq!(start["matcher"].as_str(), Some(hooks::SESSION_START));
    let start = start["hooks"][0]["command"].as_str().unwrap();
    assert!(start.contains(" agent-state idle "), "{start}");
    for event in ["UserPromptSubmit", "PostToolUse", "Notification"] {
        let groups = settings["hooks"][event].to_string();
        assert!(!groups.contains("agent-state"), "{groups}");
    }
    // A chat belongs to no workspace and finishes no assignment.
    assert!(settings.get("permissions").is_none(), "{settings}");
    assert!(settings.get("autoMode").is_none(), "{settings}");
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
    // The hooks are the same; the wrapper's merge adds the proxy's URL.
    assert_eq!(
        Claude.hook_args(WORKSPACE, true).unwrap(),
        Claude.hook_args(WORKSPACE, false).unwrap()
    );
    let mut args = Claude.hook_args(WORKSPACE, true).unwrap();
    // The user's own auto-mode entries stay beside Shoal's.
    args.extend([
        "--settings".into(),
        json!({"autoMode": {"environment": ["$defaults", "mine"]}})
            .to_string()
            .into(),
    ]);
    let home = tempfile::tempdir().unwrap();
    let environment = AgentEnvironment {
        home: home.path(),
        configured: &Default::default(),
        inherited: |_| None,
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
    let environment = settings["autoMode"]["environment"].as_array().unwrap();
    assert_eq!(environment.len(), 4, "{environment:?}");
    assert_eq!(environment[0], "$defaults");
    assert!(
        environment[1]
            .as_str()
            .unwrap()
            .contains("Git branch `fix-login-redirect`")
    );
    assert_eq!(environment[2..], [json!("$defaults"), json!("mine")]);
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

#[test]
fn the_claude_app_link_encodes_the_folder_and_takes_no_arguments() {
    assert_eq!(
        percent_encode("/a b/ä+%_.-~".as_bytes()),
        "%2Fa%20b%2F%C3%A4%2B%25_.-~"
    );
    assert_eq!(
        new_session(Path::new("/work/x y"), Vec::new()),
        [OsString::from("claude://code/new?folder=%2Fwork%2Fx%20y")]
    );
    assert_eq!(
        macos_without_arguments(&[]).is_ok(),
        cfg!(target_os = "macos")
    );
    assert!(macos_without_arguments(&["--model".into()]).is_err());
}
