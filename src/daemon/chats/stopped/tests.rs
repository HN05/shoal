use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::{chats::saved::tests::running_chat, test_support::manager},
    process::identity,
};

#[tokio::test]
async fn a_stop_of_a_chat_removed_meanwhile_records_nothing_and_goes_on() {
    let (root, manager) = manager().await;
    let (_repo, chat, process) = running_chat(root.path(), &manager).await;
    let agent = identity::capture(process.id()).unwrap().unwrap();
    // A listing finds the agent exited after a stop round signalled it, and
    // the chat is removed before the next round.
    process.kill();
    manager.stop_finished_chats().await.unwrap();
    manager
        .remove_chat(&SpaceSelector::id(&chat.id), None)
        .await
        .unwrap();
    assert!(manager.chat_by_id(&chat.id).await.unwrap().is_none());

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
