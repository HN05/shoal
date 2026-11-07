use super::*;
use crate::agents::tests::WORKSPACE;

/// The settings `args` pass, given as JSON.
fn settings_of(args: &[OsString]) -> Value {
    assert_eq!(args[0], "--settings");
    serde_json::from_str(args[1].to_str().unwrap()).unwrap()
}

#[test]
fn a_workspace_agent_may_not_stop_itself_without_a_check() {
    let settings = settings_of(&Claude.hook_args(WORKSPACE.into(), false).unwrap());
    let allow = settings["permissions"]["allow"].as_array().unwrap();
    assert!(
        allow.contains(&json!("Bash(shoal stop --leftovers)")),
        "{allow:?}"
    );
    // Plain stop would stop the agent itself.
    for rule in ["Bash(shoal stop)", "Bash(shoal stop *)"] {
        assert!(!allow.contains(&json!(rule)), "{rule}: {allow:?}");
    }
}

#[test]
fn auto_mode_learns_that_the_workspace_branch_is_the_agents_own() {
    let settings = settings_of(&Claude.hook_args(WORKSPACE.into(), false).unwrap());
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
    let settings = settings_of(&Claude.hook_args(detached.into(), false).unwrap());
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
fn a_chat_agent_runs_a_workspace_agents_hooks_without_what_needs_a_worktree() {
    let chat = settings_of(&Claude.hook_args(AgentSpace::Chat, false).unwrap());
    let workspace = settings_of(&Claude.hook_args(WORKSPACE.into(), false).unwrap());
    // It delivers messages, reports its turn's state and wakes as a
    // workspace's agent does.
    let mut shared = workspace["hooks"].as_object().unwrap().clone();
    for event in ["WorktreeCreate", "WorktreeRemove", "SubagentStart"] {
        assert!(shared.remove(event).is_some(), "{event}");
    }
    assert_eq!(chat["hooks"], Value::Object(shared));
    // It may run the commands that finish its assignment, but has no branch.
    assert_eq!(chat["permissions"], workspace["permissions"]);
    assert!(chat.get("autoMode").is_none(), "{chat}");
}

#[test]
fn proxied_settings_send_api_requests_to_the_proxy() {
    // The hooks are the same; the wrapper's merge adds the proxy's URL.
    assert_eq!(
        Claude.hook_args(WORKSPACE.into(), true).unwrap(),
        Claude.hook_args(WORKSPACE.into(), false).unwrap()
    );
    let mut args = Claude.hook_args(WORKSPACE.into(), true).unwrap();
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
