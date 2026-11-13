use super::*;
use crate::daemon::test_support::{chat, manager, repository};
use crate::execution::recovery::{Saved, SavedExt};

/// Record that the upgrade stopped the idle agent of `chat`, which Shoal
/// had `saved`, to come back on its own when `automatic`.
async fn upgrade(manager: &Manager, chat: &Chat, saved: bool, automatic: bool) {
    manager.end_executions(&chat.id).await;
    let id = chat.id.clone();
    (manager.store)
        .write(move |tx| {
            tx.execute(
                "INSERT INTO upgraded_chat_agents(chat_id,saved,automatic,stopped_by,turn)
                 VALUES (?1,?2,?3,NULL,'idle')",
                rusqlite::params![id, saved, automatic],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn an_upgraded_chats_agent_is_saved_as_a_stopped_agent_with_its_session() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let [resumed, automatic, exited] = ["resumed", "automatic", "exited"];
    let mut chats = Vec::new();
    for name in [resumed, automatic, exited] {
        let talk = chat(&manager, name, "chat", Some(&repo), None).await;
        manager
            .record_chat_session(&talk.id, format!("{name}-session").as_str().into())
            .await
            .unwrap();
        chats.push(manager.chat_by_id(&talk.id).await.unwrap().unwrap());
    }
    upgrade(&manager, &chats[0], true, false).await;
    upgrade(&manager, &chats[1], true, true).await;
    upgrade(&manager, &chats[2], false, false).await;

    manager.save_upgraded_chat_agents().await.unwrap();

    let saved = |chat: &Chat| Saved::load(&manager.paths, &chat.id).unwrap();
    // Saved for `shoal resume`, with the session and turn it had.
    let [agent] = saved(&chats[0]).agents.try_into().unwrap();
    let record = Record::load(&agent.path).unwrap().unwrap();
    assert_eq!(record.session, Some("resumed-session".into()));
    assert_eq!(record.turn, Some(AgentState::Idle));
    // Saved to come back on its own, it asks the agent queue to restore it.
    let [agent] = saved(&chats[1]).agents.try_into().unwrap();
    let requests = recovery::restore_requests(&manager.paths, &chats[1].id).unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, agent.id);
    assert!(
        recovery::restore_requests(&manager.paths, &chats[0].id)
            .unwrap()
            .is_empty()
    );
    // One that exited keeps its session as its last.
    assert!(saved(&chats[2]).agents.is_empty());
    let [last] = LastSession::all(&manager.paths, &chats[2].id)
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(last.session, Some("exited-session".into()));

    let left: i64 = (manager.store)
        .run(|db| {
            Ok(
                db.query_row("SELECT COUNT(*) FROM upgraded_chat_agents", [], |row| {
                    row.get(0)
                })?,
            )
        })
        .await
        .unwrap();
    assert_eq!(left, 0);
}
