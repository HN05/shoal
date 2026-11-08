use super::*;
use crate::client::space_ids::SpaceId;
use crate::state::ChatRestore;

fn chat(session: Option<&str>, restore: Option<ChatRestore>) -> Chat {
    serde_json::from_value(serde_json::json!({
        "id": "c1",
        "repository_id": "repo",
        "name": "talk",
        "agent": "claude",
        "pid": 1,
        "started_at": 0,
        "session": session,
        "restore": restore,
    }))
    .unwrap()
}

fn run(name: &str) -> Option<String> {
    Some(name.to_owned())
}

#[tokio::test]
async fn a_request_for_a_chat_restarting_joins_the_restart_in_progress() {
    let agents = ChatAgents::default();
    let running = chat(Some("s1"), None);

    let first = agents
        .request(&SpaceId::from("c1"), Some(&running), true, run("a"))
        .unwrap();
    assert_eq!(
        first.starts.as_ref().map(|chat| chat.id.as_str()),
        Some("c1")
    );
    // The restart stopped the chat, which a joining request still finds.
    let joined = agents
        .request(&SpaceId::from("c1"), None, false, run("b"))
        .unwrap();
    assert!(joined.starts.is_none());
    // Saved by the restart, the chat lists as stopped, and still restorable
    // so that a run joins it.
    let listed = agents.running(vec![(chat(Some("s1"), Some(ChatRestore::Resume)), false)]);
    assert!(listed[0].restorable);
    let pending = listed[0].restart.clone().unwrap();
    assert_eq!(pending.run, run("a"));
    assert_eq!(pending.joined, vec!["b".to_owned()]);

    agents.finish(&SpaceId::from("c1"), &Ok(None));
    first.answer.await.unwrap().unwrap();
    joined.answer.await.unwrap().unwrap();
    let listed = agents.running(vec![(running.clone(), true)]);
    assert_eq!(listed[0].restarted_by, vec!["a".to_owned(), "b".to_owned()]);
    assert_eq!(listed[0].restart, None);

    // Another command of a run that restarted it starts no other restart.
    let again = agents
        .request(&SpaceId::from("c1"), Some(&running), true, run("a"))
        .unwrap();
    assert!(again.starts.is_none());
    again.answer.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_late_join_answers_as_the_restart_ended() {
    let agents = ChatAgents::default();
    let first = agents.request(
        &SpaceId::from("c1"),
        Some(&chat(Some("s1"), None)),
        true,
        run("a"),
    );
    agents.finish(&SpaceId::from("c1"), &Err("no terminal host".into()));
    assert!(first.unwrap().answer.await.unwrap().is_err());

    let ended = agents
        .join(&SpaceId::from("c1"), "talk", &run("a"), run("b"))
        .unwrap_err();
    assert_eq!(ended, Err("no terminal host".into()));
    assert!(
        agents
            .join(&SpaceId::from("c2"), "other", &run("a"), run("b"))
            .is_err()
    );
}

#[test]
fn only_a_running_chat_whose_agent_resumes_starts_a_restart() {
    let agents = ChatAgents::default();
    assert!(
        agents
            .request(&SpaceId::from("c1"), None, false, run("a"))
            .is_err()
    );
    assert!(
        agents
            .request(
                &SpaceId::from("c1"),
                Some(&chat(Some("s1"), None)),
                false,
                run("a")
            )
            .is_err()
    );

    let listed = agents.running(vec![
        (chat(Some("s1"), None), false),
        (chat(Some("s1"), Some(ChatRestore::Resume)), true),
    ]);
    assert_eq!(listed.len(), 1);
    assert!(!listed[0].restorable);
    assert_eq!(listed[0].execution, "c1");
}
