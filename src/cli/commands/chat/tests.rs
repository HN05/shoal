use std::ffi::OsString;

use super::*;

#[test]
fn a_chat_started_beside_keeps_its_agent_model_and_arguments() {
    let topic = Topic {
        relaunch: vec!["chat".into(), "repo".into()],
        name: "repo-chat".into(),
        asks_name: true,
        opening: None,
        directories: Vec::new(),
    };
    let agent: Agent = "codex".parse().unwrap();
    let model = ModelChoice {
        model: None,
        effort: Some("high".into()),
    };
    let args = [OsString::from("--search")];
    let beside = Beside {
        topic: &topic,
        name: "named",
        agent: &agent,
        model: &model,
        args: &args,
        session: None,
    };
    assert_eq!(
        beside.relaunch(),
        [
            "chat", "repo", "--here", "--name", "named", "--agent", "codex", "--effort", "high",
            "--", "--search",
        ]
        .map(OsString::from)
    );
    // A chat continuing a session takes it to the place it opens.
    let resumed = Beside {
        session: Some("s1"),
        ..beside
    };
    assert_eq!(
        resumed.relaunch(),
        [
            "chat",
            "repo",
            "--here",
            "--name",
            "named",
            "--agent",
            "codex",
            "--effort",
            "high",
            "--session",
            "s1",
            "--",
            "--search",
        ]
        .map(OsString::from)
    );
}
