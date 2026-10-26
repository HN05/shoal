use std::collections::BTreeMap;

use super::*;

#[test]
fn config_overrides_deliver_messages_and_report_state() {
    let hooks: BTreeMap<_, _> = Codex
        .hook_args()
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
