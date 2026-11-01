use crate::{daemon::chats::saved::tests::running_chat, process::identity, test_support::manager};

#[tokio::test]
async fn a_stop_of_a_chat_that_ended_meanwhile_records_nothing_and_goes_on() {
    let (root, manager) = manager().await;
    let (_repo, chat, process) = running_chat(root.path(), &manager).await;
    let agent = identity::capture(process.id()).unwrap().unwrap();
    // A listing finds the agent exited after a stop round signalled it, and
    // ends the chat before the next round.
    process.kill();
    manager.end_finished_chats().await.unwrap();
    assert!(manager.recorded_chat(&chat.id).await.unwrap().is_none());

    manager
        .record_chat_stopping(&chat.id, vec![agent])
        .await
        .unwrap();

    assert!(
        manager
            .stopped_chat_processes(&chat.id)
            .await
            .unwrap()
            .is_empty()
    );
}
